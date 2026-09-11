#!/usr/bin/env bash
# 在 Windows 本机对 macOS / Linux 目标做 `cargo check`，不需要对应的 SDK。
#
# 用法：scripts/xplat/check.sh [macos|linux|all] [cargo check options]
#
# 这是跨目标元数据检查，可发现部分 cfg 与依赖声明不一致的问题
# （比如 Cargo.toml 里按平台排除了某个 crate 而代码无条件使用）。
# 使用占位 C 编译器与绑定，不能替代原生 runner 的真实 build/test。
# 输出固定在 .target-xplat/metadata-only-v1，绝不复用 CARGO_TARGET_DIR。
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd -P)"
fail() { echo "xplat: $*" >&2; exit 2; }
targets=()
case "${1:-all}" in
  macos) targets=(aarch64-apple-darwin) ;;
  linux) targets=(x86_64-unknown-linux-gnu) ;;
  all) targets=(aarch64-apple-darwin x86_64-unknown-linux-gnu) ;;
  *) echo "usage: $0 [macos|linux|all]" >&2; exit 2 ;;
esac
if [ "$#" -gt 0 ]; then shift; fi
# Keep feature/package selection and diagnostics, but reject Cargo configuration
# and output overrides (including abbreviated flags) before creating any files.
check_args=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --all-targets|--all-features|--no-default-features|--lib|--bins|--examples|--tests|--benches|--keep-going|-v|-vv|--verbose|-q|--quiet)
      check_args+=("$1"); shift ;;
    -p|--package|-F|--features|-j|--jobs|--bin|--example|--test|--bench|--exclude|--message-format|--color)
      [ "$#" -gt 1 ] && [[ "$2" != -* ]] || fail "missing value for $1"
      check_args+=("$1" "$2"); shift 2 ;;
    --package=*|--features=*|--jobs=*|--bin=*|--example=*|--test=*|--bench=*|--exclude=*|--message-format=*|--color=*)
      check_args+=("$1"); shift ;;
    *) fail "unsupported check argument: $1 (target, output, profile and config overrides are disabled)" ;;
  esac
done
cd "$repo"
host_info="$("${RUSTC:-rustc}" -vV)"
host=""
while IFS= read -r line; do
  line="${line%$'\r'}"
  case "$line" in 'host: '*) host="${line#host: }" ;; esac
done <<< "$host_info"
[ -n "$host" ] || fail "rustc did not report its host target"
for target in "${targets[@]}"; do
  [ "$target" != "$host" ] || fail "refusing native target $target; use real cargo build/test"
done
target_dir="$repo/.target-xplat/metadata-only-v1"
marker="$target_dir/.nebula-metadata-only"
for dir in "$repo/.target-xplat" "$target_dir" "$marker"; do
  [ ! -L "$dir" ] || fail "refusing symlinked metadata output: $dir"
done
if [ -e "$target_dir" ]; then
  [ -d "$target_dir" ] && [ -f "$marker" ] &&
    [ "$(cat "$marker")" = 'nebula cross-target metadata only v1' ] ||
    fail "refusing existing output without its metadata-only marker: $target_dir"
else
  mkdir -p "$target_dir"
  printf '%s\n' 'nebula cross-target metadata only v1' > "$marker"
fi
cargo_target_dir="$target_dir"
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    fake="$(cygpath -w "$here/fake-cc.cmd")"
    cargo_target_dir="$(cygpath -w "$target_dir")" ;;
  *) fake="$here/fake-cc.sh" ;;
esac
check_owned_directory() {
  local resolved
  resolved="$(cd "$1" && pwd -P)"
  case "$resolved/" in "$target_dir/"*) ;; *) fail "stub output escapes metadata directory: $1" ;; esac
}
status=0
seed_media_stub() {
  # zed `media` 的 build.rs 按宿主 cfg 决定是否生成 bindings.rs，Windows 宿主
  # 交叉到 macOS 时什么都不生成。把桩放进它的 OUT_DIR（首轮 check 后才存在）。
  local root="$target_dir/$1/debug/build" dir
  [ -d "$root" ] || return 0
  check_owned_directory "$root"
  for dir in "$root"/media-*/out; do
    [ -d "$dir" ] || continue
    check_owned_directory "$dir"
    [ ! -L "$dir/bindings.rs" ] || fail "symlinked bindings output: $dir"
    if [ ! -e "$dir/bindings.rs" ]; then
      cp "$here/media-bindings-stub.rs" "$dir/bindings.rs"
    fi
  done
  # gpui_macos 用 `include_bytes!` 嵌 Metal 着色器库，同样只有 Mac 宿主能编；
  # check 不执行它，空文件即可。
  for dir in "$root"/gpui_macos-*/out; do
    [ -d "$dir" ] || continue
    check_owned_directory "$dir"
    [ ! -L "$dir/shaders.metallib" ] || fail "symlinked shader output: $dir"
    if [ ! -e "$dir/shaders.metallib" ]; then
      : > "$dir/shaders.metallib"
    fi
  done
  return 0
}
for target in "${targets[@]}"; do
  echo "== metadata-only cargo check --target $target (not a native build/test)"
  env_target="$(echo "$target" | tr '-' '_')"
  case "$target" in *apple-darwin) seed_media_stub "$target" ;; esac
  env "CARGO_TARGET_DIR=$cargo_target_dir" "CARGO_BUILD_BUILD_DIR=$cargo_target_dir" \
    "CC_${env_target}=$fake" "CXX_${env_target}=$fake" "AR_${env_target}=$fake" \
    "PKG_CONFIG_ALLOW_CROSS=1" "RUST_FONTCONFIG_DLOPEN=1" \
    cargo check --locked --offline --workspace --target "$target" \
      --target-dir "$cargo_target_dir" ${check_args[@]+"${check_args[@]}"} || status=$?
done
exit $status
