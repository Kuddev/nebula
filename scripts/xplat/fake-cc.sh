#!/usr/bin/env bash
# 假 C 编译器：只给 `cargo check --target <非本机>` 用。
#
# 交叉 check 时 cargo 仍会跑 tree-sitter / ring / libsqlite3-sys / mlua-sys 等
# crate 的 build script，它们用 cc-rs 编 C。本机没有 macOS SDK，真编不了；
# 但 check 不链接，只要 build script 成功、产出「一个文件」即可。这里对每个
# `-o <path>` 产出一个空文件，其余参数忽略。绝不能用于 build/test。
set -eu
out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "-o" ]; then out="$arg"; fi
  case "$arg" in
    --version|-v|-dumpversion|-dumpmachine) echo "fake-cc 0.0 (nebula cross-check stub)"; exit 0 ;;
    -E) echo "#define __FAKE_CC__ 1"; exit 0 ;;
  esac
  prev="$arg"
done
if [ -n "$out" ]; then : > "$out"; fi
exit 0
