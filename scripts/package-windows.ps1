param(
    [string]$Qmake = $env:QMAKE,
    [string]$VcpkgRoot = $env:VCPKG_ROOT,
    [string]$ArchiveRoot = '',
    [string]$Iscc = ""
)

$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $project

function Find-CommandPath([string]$Name) {
    $command = Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command) { return $command.Source }
    return $null
}
function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}

if (-not (Find-CommandPath 'cargo.exe')) { throw 'Rust cargo.exe was not found in PATH.' }
if (-not (Find-CommandPath 'cl.exe')) { throw 'MSVC cl.exe was not found. Run from an x64 Visual Studio Developer PowerShell.' }
if (-not $Qmake) { $Qmake = Find-CommandPath 'qmake.exe' }
if (-not $Qmake -or -not (Test-Path -LiteralPath $Qmake -PathType Leaf)) {
    throw 'Qt qmake.exe was not found. Set QMAKE or pass -Qmake with its full path.'
}
if (-not $ArchiveRoot) {
    if (-not $VcpkgRoot) { throw 'Set VCPKG_ROOT, pass -VcpkgRoot, or pass -ArchiveRoot with a prebuilt libarchive directory.' }
    $ArchiveRoot = Join-Path $VcpkgRoot 'installed\x64-windows'
}
$archiveLib = Join-Path $ArchiveRoot 'lib\libarchive.lib'
if (-not (Test-Path -LiteralPath $archiveLib -PathType Leaf)) { $archiveLib = Join-Path $ArchiveRoot 'lib\archive.lib' }
$archiveBin = Join-Path $ArchiveRoot 'bin'
if (-not (Test-Path -LiteralPath $archiveLib -PathType Leaf) -or -not (Test-Path -LiteralPath $archiveBin -PathType Container)) {
    throw "libarchive was not found under $ArchiveRoot."
}
if (-not $Iscc) { $Iscc = Find-CommandPath 'ISCC.exe' }
if (-not $Iscc) {
    $candidate = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
    if (Test-Path -LiteralPath $candidate -PathType Leaf) { $Iscc = $candidate }
}
if (-not $Iscc -or -not (Test-Path -LiteralPath $Iscc -PathType Leaf)) { throw 'Inno Setup 6 ISCC.exe was not found. Pass -Iscc with its full path.' }

$qtBin = (& $Qmake -query QT_INSTALL_BINS).Trim()
if ($LASTEXITCODE -ne 0 -or -not $qtBin) { throw 'Could not query Qt using qmake.' }
$deploy = Join-Path $qtBin 'windeployqt.exe'
if (-not (Test-Path -LiteralPath $deploy -PathType Leaf)) { throw "windeployqt.exe not found in $qtBin" }
$manifest = Get-Content -LiteralPath (Join-Path $project 'Cargo.toml') -Raw -Encoding UTF8
if ($manifest -notmatch '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"') { throw 'Could not determine package version from Cargo.toml.' }
$version = $Matches[1]
$output = Join-Path $project 'target\windows'
$stage = Join-Path $output 'stage'
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage -Force | Out-Null
$linkDir = Join-Path $output 'lib'
New-Item -ItemType Directory -Path $linkDir -Force | Out-Null
Copy-Item -LiteralPath $archiveLib -Destination (Join-Path $linkDir 'archive.lib') -Force

$env:QMAKE = (Resolve-Path -LiteralPath $Qmake).Path
$env:CARGO_TARGET_DIR = Join-Path $project 'target'
$env:LIB = "$linkDir;$env:LIB"
$env:PATH = "$qtBin;$archiveBin;$env:PATH"
Invoke-Checked 'cargo.exe' @('build', '--release', '--locked', '--target', 'x86_64-pc-windows-msvc')
$exe = Join-Path $project 'target\x86_64-pc-windows-msvc\release\sugar-archive.exe'
Copy-Item -LiteralPath $exe -Destination $stage
Copy-Item -Path (Join-Path $archiveBin '*.dll') -Destination $stage
# Some prebuilt distributions name the DLL libarchive.dll while their import
# library still asks the Windows loader for archive.dll.
if (-not (Test-Path -LiteralPath (Join-Path $stage 'archive.dll')) -and (Test-Path -LiteralPath (Join-Path $stage 'libarchive.dll'))) {
    Copy-Item -LiteralPath (Join-Path $stage 'libarchive.dll') -Destination (Join-Path $stage 'archive.dll')
}
Invoke-Checked $deploy @('--release', '--no-translations', (Join-Path $stage 'sugar-archive.exe'))
# A separately named copy exposes the Pudding CLI while keeping the GUI and
# its runtime DLLs together in the installation directory.
Copy-Item -LiteralPath (Join-Path $stage 'sugar-archive.exe') -Destination (Join-Path $stage 'sugar-archive-pudding.exe')
Invoke-Checked $Iscc @("/DAppVersion=$version", "/DStageDir=$stage", "/DOutputDir=$output", (Join-Path $project 'windows\installer.iss'))
Write-Host "Installer created in $output"
