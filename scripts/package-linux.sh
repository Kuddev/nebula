#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C.UTF-8

usage() {
  cat <<'EOF'
Usage: scripts/package-linux.sh --binary PATH --version VERSION
       [--channel preview --preview-id ID|--channel stable]
       [--output-directory DIR] [--linuxdeploy PATH] [--force]

Builds a Linux x86_64 AppImage, portable tar.gz, and Debian package from one
fresh Pebrel release binary. Preview packages require --preview-id; stable
packages use formal Pebrel metadata and x64 asset names. linuxdeploy must be
supplied explicitly or through LINUXDEPLOY; CI downloads a pinned,
SHA-verified copy.
EOF
}

binary=""
version=""
preview_id=""
channel="preview"
output_directory="dist"
linuxdeploy="${LINUXDEPLOY:-}"
force=0

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
    --channel)
      channel="${2:-}"
      shift 2
      ;;
    --output-directory)
      output_directory="${2:-}"
      shift 2
      ;;
    --linuxdeploy)
      linuxdeploy="${2:-}"
      shift 2
      ;;
    --force)
      force=1
      shift
      ;;
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

if [[ "$channel" != "preview" && "$channel" != "stable" ]]; then
  echo "unsupported package channel: $channel (expected preview or stable)" >&2
  exit 2
fi
if [[ "$channel" == "preview" && -z "$preview_id" ]]; then
  echo "Preview packages require --preview-id" >&2
  exit 2
fi
if [[ "$channel" == "stable" && -n "$preview_id" ]]; then
  echo "stable packages must not include --preview-id" >&2
  exit 2
fi
if [[ -z "$binary" || -z "$version" || -z "$linuxdeploy" ]]; then
  usage >&2
  exit 2
fi
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid Cargo package version: $version" >&2
  exit 2
fi
if [[ -n "$preview_id" && ! "$preview_id" =~ ^[0-9A-Za-z][0-9A-Za-z.-]{0,31}$ ]]; then
  echo "invalid Preview id: $preview_id" >&2
  exit 2
fi
if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
  echo "Linux Preview packages must be built natively on Linux x86_64" >&2
  exit 1
fi

for command in appstreamcli convert desktop-file-validate dpkg dpkg-deb \
  dpkg-shlibdeps gzip ldd readelf sha256sum tar; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "required packaging command is missing: $command" >&2
    exit 1
  fi
done
if [[ ! -f "$binary" || ! -x "$binary" ]]; then
  echo "Pebrel binary is missing or not executable: $binary" >&2
  exit 1
fi
if [[ ! -f "$linuxdeploy" || ! -x "$linuxdeploy" ]]; then
  echo "linuxdeploy is missing or not executable: $linuxdeploy" >&2
  exit 1
fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo="$(cd "$script_directory/.." && pwd -P)"
binary="$(cd "$(dirname "$binary")" && pwd -P)/$(basename "$binary")"
python3 "$script_directory/preview_release.py" --check-binary "$binary"
linuxdeploy="$(cd "$(dirname "$linuxdeploy")" && pwd -P)/$(basename "$linuxdeploy")"
mkdir -p "$output_directory"
output_directory="$(cd "$output_directory" && pwd -P)"

if [[ "$channel" == "stable" ]]; then
  desktop_source="$repo/packaging/linux/io.github.kuddev.pebrel.desktop"
  metainfo_source="$repo/packaging/linux/io.github.kuddev.pebrel.metainfo.xml"
  desktop_id="io.github.kuddev.pebrel"
  package_name="pebrel"
  package_description="Pebrel GPU-accelerated terminal"
  package_note=""
  asset_architecture="x64"
  release="$version"
else
  desktop_source="$repo/packaging/linux/io.github.kuddev.pebrel.preview.desktop"
  metainfo_source="$repo/packaging/linux/io.github.kuddev.pebrel.preview.metainfo.xml"
  desktop_id="io.github.kuddev.pebrel.preview"
  package_name="pebrel-preview"
  package_description="Pebrel cross-platform Preview"
  package_note=" This package is a Preview build and is not a stable release."
  asset_architecture="x86_64"
  release="$version-preview.$preview_id"
fi
icon_source="$repo/extra/logo/nebula.png"
for required in "$desktop_source" "$metainfo_source" "$icon_source" \
  "$repo/README.md" "$repo/CHANGELOG.md" "$repo/INSTALL.md" \
  "$repo/LICENSE" "$repo/THIRD-PARTY-NOTICES" \
  "$repo/licenses/LICENSE-LUA" "$repo/licenses/LICENSE-MLUA" \
  "$repo/licenses/LICENSE-LATIN-MODERN-MATH" \
  "$repo/extra/completions/pebrel.bash" \
  "$repo/extra/completions/pebrel.fish" \
  "$repo/extra/completions/_pebrel"; do
  if [[ ! -f "$required" ]]; then
    echo "required package input is missing: $required" >&2
    exit 1
  fi
done

help_text="$("$binary" --help 2>&1)"
if [[ "$help_text" != *"--gpui"* ]]; then
  echo "refusing to package a non-GPUI Pebrel binary" >&2
  exit 1
fi
version_text="$("$binary" --version 2>&1)"
if [[ "$version_text" != *"$version"* ]]; then
  echo "binary version does not match package version $version: $version_text" >&2
  exit 1
fi
if ! readelf -h "$binary" | grep -Fq "Advanced Micro Devices X86-64"; then
  echo "Pebrel binary is not an x86_64 ELF executable" >&2
  exit 1
fi
ldd_output="$(ldd "$binary")"
if grep -Fq "not found" <<<"$ldd_output"; then
  echo "Pebrel has unresolved native dependencies:" >&2
  echo "$ldd_output" >&2
  exit 1
fi

mapfile -t glibc_versions < <(
  readelf --version-info "$binary" |
    sed -n 's/.*Name: GLIBC_\([0-9][0-9.]*\).*/\1/p' |
    sort -Vu
)
if [[ ${#glibc_versions[@]} -eq 0 ]]; then
  echo "could not determine Pebrel's required GLIBC version" >&2
  exit 1
fi
required_glibc="${glibc_versions[${#glibc_versions[@]} - 1]}"
if dpkg --compare-versions "$required_glibc" gt "2.35"; then
  echo "Pebrel requires GLIBC_$required_glibc; Preview baseline is at most GLIBC_2.35" >&2
  exit 1
fi

appimage_path="$output_directory/Pebrel-v$release-linux-$asset_architecture.AppImage"
tar_path="$output_directory/Pebrel-v$release-linux-$asset_architecture.tar.gz"
deb_path="$output_directory/Pebrel-v$release-linux-$asset_architecture.deb"
outputs=("$appimage_path" "$tar_path" "$deb_path")
for output in "${outputs[@]}"; do
  if [[ -e "$output" && $force -ne 1 ]]; then
    echo "package already exists: $output (pass --force to replace it)" >&2
    exit 1
  fi
done
if [[ $force -eq 1 ]]; then
  rm -f -- "${outputs[@]}"
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/pebrel-linux-package.XXXXXX")"
cleanup() {
  if [[ -n "${work:-}" && -d "$work" ]]; then
    rm -rf -- "$work"
  fi
}
trap cleanup EXIT

common_root="$work/common"
appdir="$work/Pebrel.AppDir"
deb_root="$work/debian/$package_name"
mkdir -p \
  "$common_root/usr/bin" \
  "$common_root/usr/share/applications" \
  "$common_root/usr/share/icons/hicolor/256x256/apps" \
  "$common_root/usr/share/metainfo" \
  "$common_root/usr/share/doc/$package_name/licenses" \
  "$common_root/usr/share/bash-completion/completions" \
  "$common_root/usr/share/fish/vendor_completions.d" \
  "$common_root/usr/share/zsh/vendor-completions"

install -m 0755 "$binary" "$common_root/usr/bin/pebrel"
install -m 0644 "$desktop_source" \
  "$common_root/usr/share/applications/$desktop_id.desktop"
install -m 0644 "$metainfo_source" \
  "$common_root/usr/share/metainfo/$desktop_id.metainfo.xml"
convert "$icon_source" -resize 256x256 \
  "$common_root/usr/share/icons/hicolor/256x256/apps/$desktop_id.png"
install -m 0644 "$repo/README.md" "$common_root/usr/share/doc/$package_name/README.md"
install -m 0644 "$repo/CHANGELOG.md" "$common_root/usr/share/doc/$package_name/CHANGELOG.md"
install -m 0644 "$repo/INSTALL.md" "$common_root/usr/share/doc/$package_name/INSTALL.md"
install -m 0644 "$repo/LICENSE" "$common_root/usr/share/doc/$package_name/licenses/LICENSE"
install -m 0644 "$repo/LICENSE" "$common_root/usr/share/doc/$package_name/copyright"
install -m 0644 "$repo/THIRD-PARTY-NOTICES" \
  "$common_root/usr/share/doc/$package_name/licenses/THIRD-PARTY-NOTICES"
install -m 0644 "$repo/licenses/LICENSE-LUA" \
  "$common_root/usr/share/doc/$package_name/licenses/LICENSE-LUA"
install -m 0644 "$repo/licenses/LICENSE-MLUA" \
  "$common_root/usr/share/doc/$package_name/licenses/LICENSE-MLUA"
install -m 0644 "$repo/licenses/LICENSE-LATIN-MODERN-MATH" \
  "$common_root/usr/share/doc/$package_name/licenses/LICENSE-LATIN-MODERN-MATH"
install -m 0644 "$repo/extra/completions/pebrel.bash" \
  "$common_root/usr/share/bash-completion/completions/pebrel"
install -m 0644 "$repo/extra/completions/pebrel.fish" \
  "$common_root/usr/share/fish/vendor_completions.d/pebrel.fish"
install -m 0644 "$repo/extra/completions/_pebrel" \
  "$common_root/usr/share/zsh/vendor-completions/_pebrel"

desktop-file-validate \
  "$common_root/usr/share/applications/$desktop_id.desktop"
appstreamcli validate --no-net \
  "$common_root/usr/share/metainfo/$desktop_id.metainfo.xml"

cp -a "$common_root/." "$appdir/"
tool_output="$work/linuxdeploy-output"
mkdir -p "$tool_output"
(
  cd "$tool_output"
  ARCH=x86_64 \
  APPIMAGE_EXTRACT_AND_RUN=1 \
  NO_STRIP=1 \
    "$linuxdeploy" \
      --appdir "$appdir" \
      --executable "$appdir/usr/bin/pebrel" \
      --desktop-file "$appdir/usr/share/applications/$desktop_id.desktop" \
      --icon-file "$appdir/usr/share/icons/hicolor/256x256/apps/$desktop_id.png" \
      --output appimage
)
mapfile -t generated_appimages < <(find "$tool_output" -maxdepth 1 -type f -name '*.AppImage' -print)
if [[ ${#generated_appimages[@]} -ne 1 ]]; then
  echo "linuxdeploy produced ${#generated_appimages[@]} AppImages; expected exactly one" >&2
  exit 1
fi
install -m 0755 "${generated_appimages[0]}" "$appimage_path"
if [[ ! -x "$appdir/AppRun" ]]; then
  echo "linuxdeploy did not create an executable AppRun launcher" >&2
  exit 1
fi

source_date_epoch="${SOURCE_DATE_EPOCH:-}"
if [[ -z "$source_date_epoch" ]]; then
  source_date_epoch="$(git -C "$repo" show -s --format=%ct HEAD)"
fi
if [[ ! "$source_date_epoch" =~ ^[0-9]+$ ]]; then
  echo "SOURCE_DATE_EPOCH must be an integer" >&2
  exit 1
fi
archive_root="Pebrel-v$release-linux-$asset_architecture"
tar \
  --sort=name \
  --mtime="@$source_date_epoch" \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  --transform="s|^Pebrel.AppDir|$archive_root|" \
  -C "$work" \
  -cf - Pebrel.AppDir | gzip -n >"$tar_path"

mkdir -p "$deb_root"
cp -a "$common_root/." "$deb_root/"
cat >"$work/debian/control" <<EOF
Source: $package_name
Section: utils
Priority: optional
Maintainer: Kuddev <Kuddev@users.noreply.github.com>
Standards-Version: 4.6.2

Package: $package_name
Architecture: any
Description: $package_description
 GPU-accelerated terminal for local and remote workflows.
EOF
dependency_line="$(
  cd "$work"
  dpkg-shlibdeps -O -e"$deb_root/usr/bin/pebrel"
)"
if [[ "$dependency_line" != shlibs:Depends=* ]]; then
  echo "dpkg-shlibdeps returned an unexpected value: $dependency_line" >&2
  exit 1
fi
dependencies="${dependency_line#shlibs:Depends=}"
installed_size="$(du -sk "$deb_root/usr" | awk '{print $1}')"
mkdir -p "$deb_root/DEBIAN"
expected_deb_version="$version"
if [[ "$channel" == "preview" ]]; then
  expected_deb_version="$version~preview.$preview_id"
fi
cat >"$deb_root/DEBIAN/control" <<EOF
Package: $package_name
Version: $expected_deb_version
Section: utils
Priority: optional
Architecture: amd64
Maintainer: Kuddev <Kuddev@users.noreply.github.com>
Installed-Size: $installed_size
Depends: $dependencies
Recommends: libsecret-tools, gnome-keyring
Homepage: https://github.com/Kuddev/pebrel
Description: $package_description
 GPU-accelerated terminal for local and remote workflows.
$package_note
EOF
chmod 0755 "$deb_root/DEBIAN"
chmod 0644 "$deb_root/DEBIAN/control"
dpkg-deb --root-owner-group --build "$deb_root" "$deb_path"

if [[ "$(dpkg-deb --field "$deb_path" Package)" != "$package_name" ]]; then
  echo "Debian package identity verification failed" >&2
  exit 1
fi
if [[ "$(dpkg-deb --field "$deb_path" Architecture)" != "amd64" ]]; then
  echo "Debian package architecture verification failed" >&2
  exit 1
fi
if [[ "$(dpkg-deb --field "$deb_path" Version)" != "$expected_deb_version" ]]; then
  echo "Debian package version verification failed" >&2
  exit 1
fi

for output in "${outputs[@]}"; do
  if [[ ! -s "$output" ]]; then
    echo "package output is missing or empty: $output" >&2
    exit 1
  fi
  sha256sum "$output"
done
echo "required GLIBC: $required_glibc"
