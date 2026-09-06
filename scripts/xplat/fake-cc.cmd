@echo off
rem 假 C 编译器（Windows 版，见 fake-cc.sh 的说明）：只给交叉 cargo check 用。
rem 对每个 "-o <path>" 产出空文件；--version / -E 打印占位并成功返回。
setlocal EnableDelayedExpansion
set "prev="
set "out="
for %%A in (%*) do (
    set "arg=%%~A"
    if "!arg!"=="--version" ( echo fake-cc 0.0 ^(nebula cross-check stub^) & exit /b 0 )
    if "!arg!"=="-dumpversion" ( echo 0.0 & exit /b 0 )
    if "!arg!"=="-dumpmachine" ( echo fake & exit /b 0 )
    if "!arg!"=="-E" ( echo #define __FAKE_CC__ 1 & exit /b 0 )
    if "!prev!"=="-o" set "out=!arg!"
    set "prev=!arg!"
)
if defined out type nul > "!out!"
exit /b 0
