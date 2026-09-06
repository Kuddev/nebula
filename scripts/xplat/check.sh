#!/usr/bin/env bash
# 在 Windows 本机对 macOS / Linux 目标做 `cargo check`，不需要对应的 SDK。
#
# 用法：scripts/xplat/check.sh [macos|linux|all]
#
# 这是跨目标元数据检查，可发现部分 cfg 与依赖声明不一致的问题
# （比如 Cargo.toml 里按平台排除了某个 crate 而代码无条件使用）。
# 使用占位 C 编译器与绑定，不能替代原生 runner 的真实 build/test。
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
targets=()
case "${1:-all}" in
  macos) targets=(aarch64-apple-darwin) ;;
  linux) targets=(x86_64-unknown-linux-gnu) ;;
  all) targets=(aarch64-apple-darwin x86_64-unknown-linux-gnu) ;;
  *) echo "usage: $0 [macos|linux|all]" >&2; exit 2 ;;
esac
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo/.target-xplat}"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) fake="$(cygpath -w "$here/fake-cc.cmd")" ;; *) fake="$here/fake-cc.sh" ;; esac
status=0
seed_media_stub() {
  # zed `media` 的 build.rs 按宿主 cfg 决定是否生成 bindings.rs，Windows 宿主
  # 交叉到 macOS 时什么都不生成。把桩放进它的 OUT_DIR（首轮 check 后才存在）。
  local root="$CARGO_TARGET_DIR/$1/debug/build" dir
  for dir in "$root"/media-*/out; do
    [ -d "$dir" ] && [ ! -f "$dir/bindings.rs" ] && cp "$here/media-bindings-stub.rs" "$dir/bindings.rs"
  done
  # gpui_macos 用 `include_bytes!` 嵌 Metal 着色器库，同样只有 Mac 宿主能编；
  # check 不执行它，空文件即可。
  for dir in "$root"/gpui_macos-*/out; do
    [ -d "$dir" ] && [ ! -f "$dir/shaders.metallib" ] && : > "$dir/shaders.metallib"
  done
  return 0
}
for target in "${targets[@]}"; do
  echo "== cargo check --target $target"
  env_target="$(echo "$target" | tr '-' '_')"
  case "$target" in *apple-darwin) seed_media_stub "$target" ;; esac
  env "CC_${env_target}=$fake" "CXX_${env_target}=$fake" "AR_${env_target}=$fake" \
    "PKG_CONFIG_ALLOW_CROSS=1" "RUST_FONTCONFIG_DLOPEN=1" \
    cargo check --offline --workspace --target "$target" "${@:2}" || status=$?
done
exit $status
