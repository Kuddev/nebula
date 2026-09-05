#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C.UTF-8

usage() {
  cat <<'EOF'
Usage: scripts/package-linux.sh --binary PATH --version VERSION --preview-id ID
       [--output-directory DIR] [--linuxdeploy PATH] [--force]

Builds a Linux x86_64 Preview AppImage, portable tar.gz, and Debian package
from one fresh Nebula release binary. linuxdeploy must be supplied explicitly
or through LINUXDEPLOY; the CI workflow downloads a pinned, SHA-verified copy.
EOF
}

binary=""
version=""
preview_id=""
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

if [[ -z "$binary" || -z "$version" || -z "$preview_id" || -z "$linuxdeploy" ]]; then
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
  echo "Nebula binary is missing or not executable: $binary" >&2
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

desktop_source="$repo/packaging/linux/io.github.kuddev.nebula.preview.desktop"
metainfo_source="$repo/packaging/linux/io.github.kuddev.nebula.preview.metainfo.xml"
icon_source="$repo/extra/logo/nebula.png"
for required in "$desktop_source" "$metainfo_source" "$icon_source" \
  "$repo/README.md" "$repo/CHANGELOG.md" "$repo/INSTALL.md" \
  "$repo/LICENSE" "$repo/THIRD-PARTY-NOTICES" \
  "$repo/licenses/LICENSE-LUA" "$repo/licenses/LICENSE-MLUA" \
  "$repo/licenses/LICENSE-LATIN-MODERN-MATH" \
  "$repo/extra/completions/nebula.bash" \
  "$repo/extra/completions/nebula.fish" \
  "$repo/extra/completions/_nebula"; do
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
if ! readelf -h "$binary" | grep -Fq "Advanced Micro Devices X86-64"; then
  echo "Nebula binary is not an x86_64 ELF executable" >&2
  exit 1
fi
ldd_output="$(ldd "$binary")"
if grep -Fq "not found" <<<"$ldd_output"; then
  echo "Nebula has unresolved native dependencies:" >&2
  echo "$ldd_output" >&2
  exit 1
fi

mapfile -t glibc_versions < <(
  readelf --version-info "$binary" |
    sed -n 's/.*Name: GLIBC_\([0-9][0-9.]*\).*/\1/p' |
    sort -Vu
)
if [[ ${#glibc_versions[@]} -eq 0 ]]; then
  echo "could not determine Nebula's required GLIBC version" >&2
  exit 1
fi
required_glibc="${glibc_versions[${#glibc_versions[@]} - 1]}"
if dpkg --compare-versions "$required_glibc" gt "2.35"; then
  echo "Nebula requires GLIBC_$required_glibc; Preview baseline is at most GLIBC_2.35" >&2
  exit 1
fi

release="$version-preview.$preview_id"
appimage_path="$output_directory/NebulaTerminal-v$release-linux-x86_64.AppImage"
tar_path="$output_directory/NebulaTerminal-v$release-linux-x86_64.tar.gz"
deb_path="$output_directory/NebulaTerminal-v$release-linux-x86_64.deb"
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

work="$(mktemp -d "${TMPDIR:-/tmp}/nebula-linux-package.XXXXXX")"
cleanup() {
  if [[ -n "${work:-}" && -d "$work" ]]; then
    rm -rf -- "$work"
  fi
}
trap cleanup EXIT

common_root="$work/common"
appdir="$work/Nebula.AppDir"
deb_root="$work/debian/nebula-terminal-preview"
mkdir -p \
  "$common_root/usr/bin" \
  "$common_root/usr/share/applications" \
  "$common_root/usr/share/icons/hicolor/256x256/apps" \
  "$common_root/usr/share/metainfo" \
  "$common_root/usr/share/doc/nebula-terminal-preview/licenses" \
  "$common_root/usr/share/bash-completion/completions" \
  "$common_root/usr/share/fish/vendor_completions.d" \
  "$common_root/usr/share/zsh/vendor-completions"

install -m 0755 "$binary" "$common_root/usr/bin/nebula"
install -m 0644 "$desktop_source" \
  "$common_root/usr/share/applications/io.github.kuddev.nebula.preview.desktop"
install -m 0644 "$metainfo_source" \
  "$common_root/usr/share/metainfo/io.github.kuddev.nebula.preview.metainfo.xml"
convert "$icon_source" -resize 256x256 \
  "$common_root/usr/share/icons/hicolor/256x256/apps/io.github.kuddev.nebula.preview.png"
install -m 0644 "$repo/README.md" "$common_root/usr/share/doc/nebula-terminal-preview/README.md"
install -m 0644 "$repo/CHANGELOG.md" "$common_root/usr/share/doc/nebula-terminal-preview/CHANGELOG.md"
install -m 0644 "$repo/INSTALL.md" "$common_root/usr/share/doc/nebula-terminal-preview/INSTALL.md"
install -m 0644 "$repo/LICENSE" "$common_root/usr/share/doc/nebula-terminal-preview/licenses/LICENSE"
install -m 0644 "$repo/LICENSE" "$common_root/usr/share/doc/nebula-terminal-preview/copyright"
install -m 0644 "$repo/THIRD-PARTY-NOTICES" \
  "$common_root/usr/share/doc/nebula-terminal-preview/licenses/THIRD-PARTY-NOTICES"
install -m 0644 "$repo/licenses/LICENSE-LUA" \
  "$common_root/usr/share/doc/nebula-terminal-preview/licenses/LICENSE-LUA"
install -m 0644 "$repo/licenses/LICENSE-MLUA" \
  "$common_root/usr/share/doc/nebula-terminal-preview/licenses/LICENSE-MLUA"
install -m 0644 "$repo/licenses/LICENSE-LATIN-MODERN-MATH" \
  "$common_root/usr/share/doc/nebula-terminal-preview/licenses/LICENSE-LATIN-MODERN-MATH"
install -m 0644 "$repo/extra/completions/nebula.bash" \
  "$common_root/usr/share/bash-completion/completions/nebula"
install -m 0644 "$repo/extra/completions/nebula.fish" \
  "$common_root/usr/share/fish/vendor_completions.d/nebula.fish"
install -m 0644 "$repo/extra/completions/_nebula" \
  "$common_root/usr/share/zsh/vendor-completions/_nebula"

desktop-file-validate \
  "$common_root/usr/share/applications/io.github.kuddev.nebula.preview.desktop"
appstreamcli validate --no-net \
  "$common_root/usr/share/metainfo/io.github.kuddev.nebula.preview.metainfo.xml"

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
      --executable "$appdir/usr/bin/nebula" \
      --desktop-file "$appdir/usr/share/applications/io.github.kuddev.nebula.preview.desktop" \
      --icon-file "$appdir/usr/share/icons/hicolor/256x256/apps/io.github.kuddev.nebula.preview.png" \
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
archive_root="NebulaTerminal-v$release-linux-x86_64"
tar \
  --sort=name \
  --mtime="@$source_date_epoch" \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  --transform="s|^Nebula.AppDir|$archive_root|" \
  -C "$work" \
  -cf - Nebula.AppDir | gzip -n >"$tar_path"

mkdir -p "$deb_root"
cp -a "$common_root/." "$deb_root/"
cat >"$work/debian/control" <<EOF
Source: nebula-terminal-preview
Section: utils
Priority: optional
Maintainer: Kuddev <Kuddev@users.noreply.github.com>
Standards-Version: 4.6.2

Package: nebula-terminal-preview
Architecture: any
Description: Nebula Terminal cross-platform Preview
 GPU-accelerated terminal for local and remote workflows.
EOF
dependency_line="$(
  cd "$work"
  dpkg-shlibdeps -O -e"$deb_root/usr/bin/nebula"
)"
if [[ "$dependency_line" != shlibs:Depends=* ]]; then
  echo "dpkg-shlibdeps returned an unexpected value: $dependency_line" >&2
  exit 1
fi
dependencies="${dependency_line#shlibs:Depends=}"
installed_size="$(du -sk "$deb_root/usr" | awk '{print $1}')"
mkdir -p "$deb_root/DEBIAN"
cat >"$deb_root/DEBIAN/control" <<EOF
Package: nebula-terminal-preview
Version: $version~preview.$preview_id
Section: utils
Priority: optional
Architecture: amd64
Maintainer: Kuddev <Kuddev@users.noreply.github.com>
Installed-Size: $installed_size
Depends: $dependencies
Recommends: libsecret-tools, gnome-keyring
Homepage: https://github.com/Kuddev/nebula
Description: Nebula Terminal cross-platform Preview
 GPU-accelerated terminal for local and remote workflows.
 This package is a Preview build and is not a stable release.
EOF
chmod 0755 "$deb_root/DEBIAN"
chmod 0644 "$deb_root/DEBIAN/control"
dpkg-deb --root-owner-group --build "$deb_root" "$deb_path"

if [[ "$(dpkg-deb --field "$deb_path" Package)" != "nebula-terminal-preview" ]]; then
  echo "Debian package identity verification failed" >&2
  exit 1
fi
if [[ "$(dpkg-deb --field "$deb_path" Architecture)" != "amd64" ]]; then
  echo "Debian package architecture verification failed" >&2
  exit 1
fi
if [[ "$(dpkg-deb --field "$deb_path" Version)" != "$version~preview.$preview_id" ]]; then
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
