@echo off
setlocal
cd /d "%~dp0"

where cl.exe >nul 2>&1
if not errorlevel 1 goto build

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" goto no_msvc
for /f "usebackq delims=" %%I in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSINSTALL=%%I"
if not defined VSINSTALL goto no_msvc
if not exist "%VSINSTALL%\VC\Auxiliary\Build\vcvars64.bat" goto no_msvc
call "%VSINSTALL%\VC\Auxiliary\Build\vcvars64.bat"
if errorlevel 1 goto failed

:build
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\package-windows.ps1"
if errorlevel 1 goto failed
echo.
echo Build succeeded. Installer is in "%~dp0target\windows".
if /i not "%~1"=="--no-pause" pause
exit /b 0

:no_msvc
echo Visual Studio C++ x64 build tools were not found. Install them and retry.
:failed
echo.
echo Build failed. See the error above.
if /i not "%~1"=="--no-pause" pause
exit /b 1
