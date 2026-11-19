[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$X64Helper, [Parameter(Mandatory=$true)][string]$Arm64Helper)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$old64 = $env:AGENTHUB_HELPER_X64
$oldArm = $env:AGENTHUB_HELPER_ARM64
Push-Location $root
try {
    $env:AGENTHUB_HELPER_X64 = (Resolve-Path $X64Helper).Path
    $env:AGENTHUB_HELPER_ARM64 = (Resolve-Path $Arm64Helper).Path
    cargo build --locked --release -p agenthub --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Windows host build failed' }
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
    $stage = Join-Path ([IO.Path]::GetTempPath()) ('agenthub-package-' + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $stage,(Join-Path $stage 'scripts') | Out-Null
    Copy-Item (Join-Path $target 'x86_64-pc-windows-msvc\release\agenthub.exe') $stage
    Copy-Item -Recurse -Force fixtures $stage
    Copy-Item -Recurse -Force docs $stage
    Copy-Item README.md $stage
    Copy-Item scripts\Test-Windows.ps1 (Join-Path $stage 'scripts')
    New-Item -Force -ItemType Directory -Path dist | Out-Null
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip=Join-Path (Resolve-Path dist) ('AgentHub-Windows-x64-' + (Get-Date -Format yyyyMMdd-HHmmss) + '.zip')
    [IO.Compression.ZipFile]::CreateFromDirectory($stage,$zip)
    Get-FileHash $zip
} finally {
    $env:AGENTHUB_HELPER_X64=$old64
    $env:AGENTHUB_HELPER_ARM64=$oldArm
    Pop-Location
}

