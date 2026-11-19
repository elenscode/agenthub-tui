[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$Distro,
    [string]$Binary = (Join-Path $PSScriptRoot '..\agenthub.exe'),
    [string]$FixtureRoot = (Join-Path $PSScriptRoot '..\fixtures'),
    [switch]$InstallHelper
)
$ErrorActionPreference = 'Stop'
$exe = (Resolve-Path $Binary).Path
$fixture = (Resolve-Path $FixtureRoot).Path
$root = Join-Path ([IO.Path]::GetTempPath()) ("agenthub-at-" + [guid]::NewGuid())
$project = Join-Path $root 'project'
$privateHome = Join-Path $root 'home'
$state = Join-Path $root 'state'
$expected = @{ Codex='0.167.0-alpha.4'; OpenCode='1.19.6'; Claude='2.1.327' }
$report = [ordered]@{ OS=[Environment]::OSVersion.VersionString; distro=$Distro; AT07='Not Run'; AT10='Not Run'; evidence=$root }
$variables = @{ AGENTHUB_STATE_DIR=$state; CODEX_HOME=(Join-Path $privateHome '.codex'); CLAUDE_CONFIG_DIR=(Join-Path $privateHome '.claude'); XDG_CONFIG_HOME=(Join-Path $privateHome '.config'); XDG_DATA_HOME=(Join-Path $root 'data'); XDG_STATE_HOME=(Join-Path $root 'xdgstate'); XDG_CACHE_HOME=(Join-Path $root 'cache'); OPENCODE_DISABLE_AUTOUPDATE='1'; CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1' }
$prior = @{}
function Run-Checked([string[]]$Items) {
    $output = & $exe @Items
    if ($LASTEXITCODE -ne 0) { throw "AgentHub exited with $LASTEXITCODE" }
    return ($output -join [Environment]::NewLine)
}
$activeTest = "AT07"
try {
    New-Item -ItemType Directory -Path $root | Out-Null
    Copy-Item -Recurse -Force (Join-Path $fixture 'home') $privateHome
    Copy-Item -Recurse -Force (Join-Path $fixture 'project') $project
    foreach ($key in $variables.Keys) {
        $prior[$key] = [Environment]::GetEnvironmentVariable($key,'Process')
        [Environment]::SetEnvironmentVariable($key,$variables[$key],'Process')
    }
    $report.DPAPI = Run-Checked @('--self-test-dpapi')
    $report.AT07 = 'Pass'
    $activeTest = 'AT10'
    $scan = (Run-Checked @('--scan','--fixture-home',$privateHome,'--project',$project)) | ConvertFrom-Json
    $report.windowsCliVersions = $scan.versions
    $kinds = @($scan.assets.kind | Sort-Object -Unique)
    if ($kinds.Count -ne 7) { throw "Expected seven asset types, found $($kinds.Count)" }
    $linuxProject = ((& wsl.exe --distribution $Distro --exec wslpath -a -u $project) -join '').Trim()
    if ($LASTEXITCODE -ne 0 -or !$linuxProject.StartsWith('/')) { throw 'Could not map the isolated project into WSL' }
    $probeArgs = @('--probe-wsl','--distro',$Distro,'--wsl-project',$linuxProject)
    if ($InstallHelper) { $probeArgs += '--approve-install' }
    $helper = (Run-Checked $probeArgs) | ConvertFrom-Json
    $report.wslCliVersions = $helper.versions
    $report.helper = $helper.environment
    $report.wslKernel = ((& wsl.exe --distribution $Distro --exec uname -srm) -join ' ').Trim()
    foreach ($provider in $expected.Keys) {
        if ($scan.versions.$provider -ne $expected[$provider] -and $helper.versions.$provider -ne $expected[$provider]) { throw "Supported $provider version was not found on Windows or selected WSL" }
    }
    Write-Host '첫 실행 마법사에서 기본 임시 프로젝트와 지정한 배포판을 선택하세요. 두 환경의 자산을 확인한 뒤 q로 종료하세요.'
    & $exe --setup --project $project --fixture-home $privateHome
    if ($LASTEXITCODE -ne 0) { throw 'Interactive wizard failed' }
    $settings = Get-Content -Raw -Encoding UTF8 (Join-Path $state 'settings-v1.json') | ConvertFrom-Json
    if ($settings.version -ne 1 -or $settings.wsl.distro -ne $Distro -or $settings.wsl.project -ne $linuxProject) { throw 'Wizard selections did not match the selected WSL project' }
    if ((Read-Host 'Windows와 WSL 자산을 모두 확인했으면 PASS 입력') -ne 'PASS') { throw 'Interactive environment check unconfirmed' }
    $report.AT10 = 'Pass'
} catch {
    $report[$activeTest] = 'Fail'
    $report.error = $_.Exception.Message
    throw
} finally {
    $report | ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 (Join-Path $root 'acceptance.json')
    foreach ($key in $prior.Keys) { [Environment]::SetEnvironmentVariable($key,$prior[$key],'Process') }
    Write-Host "Evidence: $root\acceptance.json"
}

