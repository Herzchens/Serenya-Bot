@echo off
echo ========================================================
echo   Serenya Windows Build Helper (CMake + MSVC Fix)
echo ========================================================
echo.

:: Cau hinh bien moi truong CMake ep su dung ban VS o o D
set CMAKE_GENERATOR_INSTANCE=D:\Visual Studio 2022

:: Them CMake vao PATH tam thoi neu no chua co san
set PATH=D:\CMake\bin;%PATH%

:: Khoi tao moi truong C++ Compiler (vcvars64)
echo [1/2] Initializing Visual Studio Build Environment...
call "D:\Visual Studio 2022\VC\Auxiliary\Build\vcvars64.bat"

:: Chay lenh cargo voi cac tham so truyen vao
echo.
echo [2/2] Running Cargo command...
if "%~1"=="" (
    cargo build
) else (
    cargo %*
)
