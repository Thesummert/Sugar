param([string]$VcpkgRoot = $env:VCPKG_ROOT, [string]$ArchiveRoot = '', [string]$Iscc = '')
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $project
if (-not $ArchiveRoot) { if (-not $VcpkgRoot) { throw 'Set VCPKG_ROOT or pass -ArchiveRoot.' }; $ArchiveRoot = Join-Path $VcpkgRoot 'installed\x64-windows' }
$archiveLib = Join-Path $ArchiveRoot 'lib\libarchive.lib'
if (-not (Test-Path -LiteralPath $archiveLib)) { $archiveLib = Join-Path $ArchiveRoot 'lib\archive.lib' }
$archiveBin = Join-Path $ArchiveRoot 'bin'
if (-not (Test-Path -LiteralPath $archiveLib) -or -not (Test-Path -LiteralPath $archiveBin)) { throw 'libarchive library and DLL directory are required.' }
if (-not $Iscc) { $c = Get-Command ISCC.exe -ErrorAction SilentlyContinue; if ($c) { $Iscc = $c.Source } }
if (-not $Iscc) { $Iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe' }
if (-not (Test-Path -LiteralPath $Iscc)) { throw 'Inno Setup 6 is required.' }
$manifest = Get-Content Cargo.toml -Raw -Encoding UTF8
if ($manifest -notmatch '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"') { throw 'Version not found.' }
$version = $Matches[1]
$output = Join-Path $project 'target\windows'
$stage = Join-Path $output 'stage'
if (Test-Path -LiteralPath $stage) {
    $resolvedStage = (Resolve-Path -LiteralPath $stage).Path
    $resolvedOutput = (Resolve-Path -LiteralPath $output).Path.TrimEnd('\') + '\'
    if (-not $resolvedStage.StartsWith($resolvedOutput, [System.StringComparison]::OrdinalIgnoreCase)) { throw 'Stage directory is outside the output directory.' }
    Remove-Item -LiteralPath $resolvedStage -Recurse -Force
}
New-Item -ItemType Directory -Path $stage -Force | Out-Null
$linkDir = Join-Path $output 'lib'; New-Item -ItemType Directory -Path $linkDir -Force | Out-Null
Copy-Item -LiteralPath $archiveLib -Destination (Join-Path $linkDir 'archive.lib') -Force
$env:LIBARCHIVE_ROOT = (Resolve-Path -LiteralPath $ArchiveRoot).Path
$env:LIB = "$linkDir;$env:LIB"; $env:PATH = "$archiveBin;$env:PATH"
npm.cmd ci --no-audit --no-fund; if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
npm.cmd run build; if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed' }
cargo.exe build --release --locked --target x86_64-pc-windows-msvc; if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
Copy-Item -LiteralPath 'target\x86_64-pc-windows-msvc\release\sugar-archive.exe' -Destination $stage
Copy-Item -Path (Join-Path $archiveBin '*.dll') -Destination $stage
if (-not (Test-Path (Join-Path $stage 'archive.dll')) -and (Test-Path (Join-Path $stage 'libarchive.dll'))) { Copy-Item (Join-Path $stage 'libarchive.dll') (Join-Path $stage 'archive.dll') }
Copy-Item (Join-Path $stage 'sugar-archive.exe') (Join-Path $stage 'sugar-archive-pudding.exe')
& $Iscc "/DAppVersion=$version" "/DStageDir=$stage" "/DOutputDir=$output" (Join-Path $project 'windows\installer.iss')
if ($LASTEXITCODE -ne 0) { throw 'Installer build failed' }
Write-Host "Installer created in $output"
