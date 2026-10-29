# Only runs on an ephemeral GitHub Actions Windows runner. Never use a developer
# account or its real tool homes. Consumes an unsigned CI installer; no signing.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string]$ArtifactDirectory,
    [ValidateRange(5, 60)] [int]$LaunchSeconds = 30
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:CI -ne 'true' -or -not $env:RUNNER_TEMP) {
    throw 'Packaged smoke is restricted to ephemeral GitHub Actions Windows runners.'
}

$artifactDir = (Resolve-Path -LiteralPath $ArtifactDirectory).Path
$manifest = Get-Content -Raw -LiteralPath (Join-Path $artifactDir 'build-manifest.json') | ConvertFrom-Json
if ($manifest.target -ne 'x86_64-pc-windows-msvc' -or $manifest.mode -ne 'ci-pending-local-finalization') {
    throw 'Expected a pending Windows x64 artifact.'
}
$installer = Join-Path $artifactDir $manifest.installerArtifact.name
$expectedMain = Join-Path $artifactDir $manifest.mainExecutableArtifact.name
$smokeRoot = Join-Path $env:RUNNER_TEMP ('osir-packaged-smoke-' + [guid]::NewGuid().ToString('N'))
$installDir = Join-Path $smokeRoot 'installed'
$isolatedToolHome = Join-Path $smokeRoot 'tool-home'
$isolatedAppData = Join-Path $smokeRoot 'roaming'
$isolatedLocalAppData = Join-Path $smokeRoot 'local'
$mainExe = Join-Path $installDir 'osir_client.exe'
$uninstaller = Join-Path $installDir 'uninstall.exe'
$ownedLaunch = $null
$stage = 'preflight'
$failure = $null
$report = [ordered]@{
    schemaVersion = 1
    passed = $false
    platform = 'win32'
    target = $manifest.target
    version = $manifest.version
    sourceSha256 = $manifest.sourceProvenance.sourceSha256
    gitHead = $manifest.sourceProvenance.git.head
    startedAt = [DateTime]::UtcNow.ToString('o')
    isolation = 'ephemeral-runner-tool-home-and-appdata'
    authenticode = 'unsigned'
    stages = [ordered]@{}
}

function Assert-Smoke([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "[$script:stage] $Message" }
}

function Get-Hash([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Start-Isolated([string]$Path, [string]$Arguments = '') {
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Path
    $start.Arguments = $Arguments
    $start.WorkingDirectory = $smokeRoot
    $start.UseShellExecute = $false
    $start.Environment['CC_SWITCH_TEST_HOME'] = $isolatedToolHome
    $start.Environment['OSIR_TEST_HOME'] = $isolatedToolHome
    $start.Environment['APPDATA'] = $isolatedAppData
    $start.Environment['LOCALAPPDATA'] = $isolatedLocalAppData
    return [System.Diagnostics.Process]::Start($start)
}

function Stop-OwnedApp {
    if ($null -ne $script:ownedLaunch) {
        try {
            if (-not $script:ownedLaunch.HasExited) {
                $script:ownedLaunch.Kill($true)
                [void]$script:ownedLaunch.WaitForExit(10000)
            }
        } catch { Write-Warning "Owned launch cleanup: $($_.Exception.Message)" }
    }
    # NSIS UPDATE can start a child itself. Only this unique install path is ours.
    Get-Process -Name 'osir_client' -ErrorAction SilentlyContinue | ForEach-Object {
        try {
            if ($_.Path -and [string]::Equals($_.Path, $mainExe, [StringComparison]::OrdinalIgnoreCase)) {
                $_.Kill($true)
                [void]$_.WaitForExit(10000)
            }
        } catch { Write-Warning "Owned installed process cleanup: $($_.Exception.Message)" }
    }
}

function Invoke-Installer([string]$Path, [string]$Arguments) {
    $process = Start-Isolated $Path $Arguments
    try {
        if (-not $process.WaitForExit(180000)) {
            $process.Kill($true)
            throw "[$script:stage] Installer timed out."
        }
        Assert-Smoke ($process.ExitCode -eq 0) "Installer exited $($process.ExitCode)."
    } finally { $process.Dispose() }
}

function Assert-Installed {
    Assert-Smoke (Test-Path -LiteralPath $mainExe -PathType Leaf) 'Installed main executable is missing.'
    Assert-Smoke (Test-Path -LiteralPath $uninstaller -PathType Leaf) 'Uninstaller is missing.'
    Assert-Smoke ((Get-Hash $mainExe) -eq $manifest.mainExecutableArtifact.sha256) 'Installed main executable differs from the CI payload.'
    $signature = Get-AuthenticodeSignature -LiteralPath $mainExe
    Assert-Smoke ($signature.Status.ToString() -eq 'NotSigned') 'Main executable unexpectedly has an Authenticode signature.'
    $peJson = & node (Join-Path $PSScriptRoot 'osir-pe-verify.mjs') $mainExe x86_64
    Assert-Smoke ($LASTEXITCODE -eq 0) 'Installed PE architecture verification failed.'
    $pe = ($peJson -join "`n") | ConvertFrom-Json
    $versionInfo = (Get-Item -LiteralPath $mainExe).VersionInfo
    Assert-Smoke ($versionInfo.ProductVersion -eq $manifest.version -or $versionInfo.FileVersion -eq ($manifest.version + '.0')) 'Installed version differs from the manifest.'
    return [ordered]@{
        passed = $true
        sha256 = Get-Hash $mainExe
        architecture = $pe.architecture
        fileVersion = $versionInfo.FileVersion
        productVersion = $versionInfo.ProductVersion
        authenticodeStatus = $signature.Status.ToString()
    }
}

function Wait-Uninstalled {
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while ((Test-Path -LiteralPath $mainExe) -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 250
    }
    Assert-Smoke (-not (Test-Path -LiteralPath $mainExe)) 'Main executable remains after uninstall.'
}

try {
    foreach ($path in @($smokeRoot, $isolatedToolHome, $isolatedAppData, $isolatedLocalAppData)) {
        [void](New-Item -ItemType Directory -Path $path)
    }
    Assert-Smoke ((Get-Hash $installer) -eq $manifest.installerArtifact.sha256) 'Installer hash differs from the manifest.'
    Assert-Smoke ((Get-Hash $expectedMain) -eq $manifest.mainExecutableArtifact.sha256) 'Main payload hash differs from the manifest.'
    $installerSignature = Get-AuthenticodeSignature -LiteralPath $installer
    Assert-Smoke ($installerSignature.Status.ToString() -eq 'NotSigned') 'Installer unexpectedly has an Authenticode signature.'
    $report.installerSha256 = Get-Hash $installer
    $report.installerAuthenticodeStatus = $installerSignature.Status.ToString()
    $stage = 'install'
    # NSIS requires /D as the last raw argument, without quotation marks even
    # when the destination contains spaces. It consumes the remaining command.
    Invoke-Installer $installer ("/S /D=" + $installDir)
    $report.stages.install = Assert-Installed

    $stage = 'launch'
    $ownedLaunch = Start-Isolated $mainExe
    $deadline = [DateTime]::UtcNow.AddSeconds($LaunchSeconds)
    $windowFound = $false
    while ([DateTime]::UtcNow -lt $deadline) {
        $ownedLaunch.Refresh()
        Assert-Smoke (-not $ownedLaunch.HasExited) 'Installed application exited before displaying its window.'
        if ($ownedLaunch.MainWindowHandle -ne [IntPtr]::Zero) { $windowFound = $true; break }
        Start-Sleep -Milliseconds 250
    }
    Assert-Smoke $windowFound 'Installed application did not display a main window.'
    # Observe briefly after the window appears; a momentary handle is insufficient.
    Start-Sleep -Seconds 3
    $ownedLaunch.Refresh()
    Assert-Smoke (-not $ownedLaunch.HasExited -and $ownedLaunch.MainWindowHandle -ne [IntPtr]::Zero) 'Application window closed immediately after launch.'
    $report.stages.launch = [ordered]@{
        passed = $true
        processId = $ownedLaunch.Id
        windowObserved = $true
        windowTitle = $ownedLaunch.MainWindowTitle
        remainedRunning = $true
    }
    Stop-OwnedApp

    $stage = 'upgrade'
    Invoke-Installer $installer ("/S /UPDATE /D=" + $installDir)
    $report.stages.upgrade = Assert-Installed
    $report.stages.upgrade.kind = 'same-version-reinstall-update-path'
    Stop-OwnedApp

    $stage = 'uninstall'
    Invoke-Installer $uninstaller '/S'
    Wait-Uninstalled
    $report.stages.uninstall = [ordered]@{ passed = $true; mainExecutableRemoved = $true }
    $report.passed = $true
} catch {
    $failure = $_
    $report.failedStage = $stage
    $report.failure = $_.Exception.Message
    Write-Host "::error::[$stage] $($_.Exception.Message)"
} finally {
    Stop-OwnedApp
    if (Test-Path -LiteralPath $uninstaller -PathType Leaf) {
        try { Invoke-Installer $uninstaller '/S'; Wait-Uninstalled }
        catch { Write-Warning "Smoke cleanup: $($_.Exception.Message)" }
    }
    $report.completedAt = [DateTime]::UtcNow.ToString('o')
    $report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $artifactDir 'windows-packaged-smoke.json') -Encoding utf8NoBOM
    # This exact UUID directory was created above; no user profile is removed.
    if (Test-Path -LiteralPath $smokeRoot) { Remove-Item -LiteralPath $smokeRoot -Recurse -Force -ErrorAction SilentlyContinue }
    if ($null -ne $ownedLaunch) { $ownedLaunch.Dispose() }
}
if ($null -ne $failure) { throw $failure }
Write-Host 'Packaged lifecycle passed: install, real window launch, UPDATE reinstall, uninstall.'
