#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=en_US.UTF-8

usage() {
  cat <<'EOF'
Usage: scripts/package-macos.sh --binary PATH --version VERSION --preview-id ID
       --architecture aarch64|x86_64 --build-number NUMBER
       [--output-directory DIR] [--force]
       [--sign-identity ID --notary-profile PROFILE [--signing-keychain PATH]]

Builds an ad-hoc-signed Nebula Terminal Preview .app and packages it in a DMG.
Developer ID mode requires both signing and notarization; failures never
silently downgrade to an ad-hoc signature.
EOF
}

binary=""
version=""
preview_id=""
architecture=""
build_number=""
output_directory="dist"
force=0
sign_identity=""
notary_profile=""
signing_keychain=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)
      binary="${2:-}"
      shift 2
      ;;
    --version)
      version="${2:-}"
      shift 2
      ;;
    --preview-id)
      preview_id="${2:-}"
      shift 2
      ;;
    --architecture)
      architecture="${2:-}"
      shift 2
      ;;
    --build-number)
      build_number="${2:-}"
      shift 2
      ;;
    --output-directory)
      output_directory="${2:-}"
      shift 2
      ;;
    --force)
      force=1
      shift
      ;;
    --sign-identity) sign_identity="${2:-}"; shift 2 ;;
    --notary-profile) notary_profile="${2:-}"; shift 2 ;;
    --signing-keychain) signing_keychain="${2:-}"; shift 2 ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -n "$sign_identity" || -n "$notary_profile" || -n "$signing_keychain" ]]; then
  if [[ "$sign_identity" != "Developer ID Application: "* || -z "$notary_profile" ]]; then
    echo "Developer ID mode requires a Developer ID Application identity and notary profile" >&2
    exit 2
  fi
fi

if [[ -z "$binary" || -z "$version" || -z "$preview_id" || \
      -z "$architecture" || -z "$build_number" ]]; then
  usage >&2
  exit 2
fi
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid Cargo package version: $version" >&2
  exit 2
fi
if [[ ! "$preview_id" =~ ^[0-9A-Za-z][0-9A-Za-z.-]{0,31}$ ]]; then
  echo "invalid Preview id: $preview_id" >&2
  exit 2
fi
if [[ "$architecture" != "aarch64" && "$architecture" != "x86_64" ]]; then
  echo "unsupported macOS package architecture: $architecture" >&2
  exit 2
fi
if [[ ! "$build_number" =~ ^[1-9][0-9]*$ ]]; then
  echo "CFBundleVersion build number must be a positive integer" >&2
  exit 2
fi
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS Preview packages must be built on a native macOS runner" >&2
  exit 1
fi

expected_uname="arm64"
if [[ "$architecture" == "x86_64" ]]; then
  expected_uname="x86_64"
fi
if [[ "$(uname -m)" != "$expected_uname" ]]; then
  echo "runner architecture $(uname -m) does not match requested $architecture" >&2
  exit 1
fi
for command in codesign hdiutil iconutil lipo otool plutil shasum sips; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "required packaging command is missing: $command" >&2
    exit 1
  fi
done
if [[ ! -f "$binary" || ! -x "$binary" ]]; then
  echo "Nebula binary is missing or not executable: $binary" >&2
  exit 1
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo="$(cd "$script_directory/.." && pwd -P)"
binary="$(cd "$(dirname "$binary")" && pwd -P)/$(basename "$binary")"
python3 "$script_directory/preview_release.py" --check-binary "$binary"
mkdir -p "$output_directory"
output_directory="$(cd "$output_directory" && pwd -P)"

plist_source="$repo/packaging/macos/Info.plist"
icon_source="$repo/extra/logo/nebula.png"
for required in "$plist_source" "$icon_source" "$repo/README.md" \
  "$repo/CHANGELOG.md" "$repo/INSTALL.md" "$repo/LICENSE" \
  "$repo/THIRD-PARTY-NOTICES" "$repo/licenses/LICENSE-LUA" \
  "$repo/licenses/LICENSE-MLUA" "$repo/licenses/LICENSE-LATIN-MODERN-MATH"; do
  if [[ ! -f "$required" ]]; then
    echo "required package input is missing: $required" >&2
    exit 1
  fi
done

help_text="$("$binary" --help 2>&1)"
if [[ "$help_text" != *"--gpui"* ]]; then
  echo "refusing to package a non-GPUI Nebula binary" >&2
  exit 1
fi
version_text="$("$binary" --version 2>&1)"
if [[ "$version_text" != *"$version"* ]]; then
  echo "binary version does not match package version $version: $version_text" >&2
  exit 1
fi
binary_architectures="$(lipo -archs "$binary")"
if [[ "$binary_architectures" != "$expected_uname" ]]; then
  echo "binary architecture is $binary_architectures; expected only $expected_uname" >&2
  exit 1
fi
while IFS= read -r dependency; do
  case "$dependency" in
    /usr/lib/*|/System/Library/*) ;;
    *) echo "non-system Mach-O dependency is not bundled: $dependency" >&2; exit 1 ;;
  esac
done < <(otool -L "$binary" | tail -n +2 | awk '{print $1}')

release="$version-preview.$preview_id"
dmg_path="$output_directory/NebulaTerminal-v$release-macos-$architecture.dmg"
if [[ -e "$dmg_path" && $force -ne 1 ]]; then
  echo "package already exists: $dmg_path (pass --force to replace it)" >&2
  exit 1
fi
if [[ $force -eq 1 ]]; then
  rm -f -- "$dmg_path"
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/nebula-macos-package.XXXXXX")"
cleanup() {
  if [[ -n "${work:-}" && -d "$work" ]]; then
    rm -rf -- "$work"
  fi
}
trap cleanup EXIT

stage="$work/dmg-root"
app="$stage/Nebula Terminal Preview.app"
contents="$app/Contents"
resources="$contents/Resources"
mkdir -p "$contents/MacOS" "$resources/docs" "$resources/licenses"
install -m 0755 "$binary" "$contents/MacOS/nebula"
install -m 0644 "$plist_source" "$contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$version" "$contents/Info.plist"
plutil -replace CFBundleVersion -string "$build_number" "$contents/Info.plist"
plutil -replace CFBundleGetInfoString -string \
  "Nebula Terminal $version Preview $preview_id" "$contents/Info.plist"
plutil -lint "$contents/Info.plist"

iconset="$work/nebula.iconset"
mkdir -p "$iconset"
while read -r pixels filename; do
  sips -z "$pixels" "$pixels" "$icon_source" --out "$iconset/$filename" >/dev/null
done <<'EOF'
16 icon_16x16.png
32 icon_16x16@2x.png
32 icon_32x32.png
64 icon_32x32@2x.png
128 icon_128x128.png
256 icon_128x128@2x.png
256 icon_256x256.png
512 icon_256x256@2x.png
512 icon_512x512.png
1024 icon_512x512@2x.png
EOF
iconutil -c icns "$iconset" -o "$resources/nebula.icns"

install -m 0644 "$repo/README.md" "$resources/docs/README.md"
install -m 0644 "$repo/CHANGELOG.md" "$resources/docs/CHANGELOG.md"
install -m 0644 "$repo/INSTALL.md" "$resources/docs/INSTALL.md"
install -m 0644 "$repo/LICENSE" "$resources/licenses/LICENSE"
install -m 0644 "$repo/THIRD-PARTY-NOTICES" "$resources/licenses/THIRD-PARTY-NOTICES"
install -m 0644 "$repo/licenses/LICENSE-LUA" "$resources/licenses/LICENSE-LUA"
install -m 0644 "$repo/licenses/LICENSE-MLUA" "$resources/licenses/LICENSE-MLUA"
install -m 0644 "$repo/licenses/LICENSE-LATIN-MODERN-MATH" \
  "$resources/licenses/LICENSE-LATIN-MODERN-MATH"

keychain_args=()
if [[ -n "$signing_keychain" ]]; then
  keychain_args=(--keychain "$signing_keychain")
fi
if [[ -n "$sign_identity" ]]; then
  codesign --force --sign "$sign_identity" --options runtime --timestamp \
    ${keychain_args[@]+"${keychain_args[@]}"} "$app"
else
  codesign --force --sign - --timestamp=none "$app"
fi
codesign --verify --deep --strict --verbose=2 "$app"
if [[ "$(plutil -extract CFBundleIdentifier raw -o - "$contents/Info.plist")" != \
      "io.github.kuddev.nebula.preview" ]]; then
  echo "application bundle identifier verification failed" >&2
  exit 1
fi
if [[ "$(lipo -archs "$contents/MacOS/nebula")" != "$expected_uname" ]]; then
  echo "packaged Mach-O architecture verification failed" >&2
  exit 1
fi

ln -s /Applications "$stage/Applications"
hdiutil create \
  -volname "Nebula Terminal Preview" \
  -srcfolder "$stage" \
  -ov \
  -format UDZO \
  "$dmg_path"
hdiutil verify "$dmg_path"
if [[ -n "$sign_identity" ]]; then
  codesign --force --sign "$sign_identity" --timestamp ${keychain_args[@]+"${keychain_args[@]}"} "$dmg_path"
  xcrun notarytool submit "$dmg_path" --keychain-profile "$notary_profile" \
    ${keychain_args[@]+"${keychain_args[@]}"} --wait --output-format json >"$work/notarization.json"
  python3 - "$work/notarization.json" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as stream:
    result = json.load(stream)
if result.get("status") != "Accepted":
    raise SystemExit(f"Apple notarization did not accept the DMG: {result.get('status')}")
PY
  xcrun stapler staple "$dmg_path"
  xcrun stapler validate "$dmg_path"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg_path"
fi
if [[ ! -s "$dmg_path" ]]; then
  echo "DMG output is missing or empty: $dmg_path" >&2
  exit 1
fi
shasum -a 256 "$dmg_path"
