param(
    [string]$Executable = '',
    [ValidatePattern('^$|^[a-fA-F0-9]{64}$')]
    [string]$ExpectedExecutableSha256 = '',
    [string]$ReportPath = ''
)
$ErrorActionPreference = 'Continue'
$configurationHome = $env:USERPROFILE
if ($env:CC_SWITCH_TEST_HOME) { $configurationHome = $env:CC_SWITCH_TEST_HOME }
$report = [ordered]@{
    schemaVersion = 1
    checkedAt = (Get-Date).ToString('o')
    powershell = $PSVersionTable.PSVersion.ToString()
    system = Get-CimInstance Win32_OperatingSystem |
        Select-Object Caption, Version, BuildNumber, OSArchitecture
}

function File-Evidence([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $null }
    $item = Get-Item -LiteralPath $Path
    [ordered]@{
        path = $item.FullName
        bytes = $item.Length
        modified = $item.LastWriteTime.ToString('o')
    }
}

function File-Sha256([string]$Path) {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($Path)
    try {
        return [BitConverter]::ToString($algorithm.ComputeHash($stream)).Replace('-', '').ToLowerInvariant()
    } finally {
        $stream.Dispose()
        $algorithm.Dispose()
    }
}

function Database-Evidence([string]$Path) {
    $item = File-Evidence $Path
    if (-not $item) { return $null }
    try {
        $stream = [IO.File]::Open($Path, [IO.FileMode]::Open,
            [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
        try {
            $header = New-Object byte[] 100
            $count = $stream.Read($header, 0, 100)
            $item.sqliteHeader = $count -eq 100 -and
                [Text.Encoding]::ASCII.GetString($header, 0, 15) -eq 'SQLite format 3'
            if ($item.sqliteHeader) {
                $item.userVersion = [long]$header[60] * 16777216 +
                    [long]$header[61] * 65536 + [long]$header[62] * 256 + $header[63]
            }
        } finally { $stream.Dispose() }
    } catch { $item.readable = $false }
    return $item
}

$records = @(
    'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
    'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
    'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
) | ForEach-Object {
    Get-ItemProperty $_ -ErrorAction SilentlyContinue
} | Where-Object { $_.DisplayName -eq 'OSIR Client' }
$report.installations = @($records | Select-Object DisplayName, DisplayVersion,
    InstallLocation, DisplayIcon, EstimatedSize)
if (-not $Executable) {
    foreach ($record in $records) {
        $candidate = Join-Path ($record.InstallLocation -replace '^"|"$', '') 'osir_client.exe'
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            $Executable = $candidate
            break
        }
    }
}
if ($Executable -and (Test-Path -LiteralPath $Executable -PathType Leaf)) {
    $Executable = (Get-Item -LiteralPath $Executable).FullName
    $report.executable = File-Evidence $Executable
    $report.executable.sha256 = File-Sha256 $Executable
    $report.executable.version = (Get-Item -LiteralPath $Executable).VersionInfo.ProductVersion
    if ($ExpectedExecutableSha256) {
        $report.executable.isExpectedBuild = $report.executable.sha256 -eq $ExpectedExecutableSha256
    }
}
$report.osirProcesses = @(Get-Process osir_client -ErrorAction SilentlyContinue |
    Select-Object Id, ProcessName, MainWindowHandle)
$report.startupTest = [ordered]@{}
if (-not $report.executable) {
    $report.startupTest.skipped = 'executable-not-found'
} elseif ($ExpectedExecutableSha256 -and -not $report.executable.isExpectedBuild) {
    $report.startupTest.skipped = 'different-build'
} elseif ($report.osirProcesses.Count) {
    $report.startupTest.skipped = 'already-running'
} elseif ($Executable -and
    (-not $ExpectedExecutableSha256 -or $report.executable.isExpectedBuild) -and
    -not @(Get-Process osir_client -ErrorAction SilentlyContinue).Count) {
    try {
        $start = New-Object Diagnostics.ProcessStartInfo
        $start.FileName = $Executable
        $start.UseShellExecute = $false
        $process = [Diagnostics.Process]::Start($start)
        $exited = $process.WaitForExit(12000)
        $process.Refresh()
        $report.startupTest.started = $true
        $report.startupTest.aliveAfter12Seconds = -not $exited
        $report.startupTest.windowObserved = $process.MainWindowHandle -ne [IntPtr]::Zero
        if ($exited) { $report.startupTest.exitCode = $process.ExitCode }
        $process.Dispose()
    } catch { $report.startupTest.started = $false }
}
$report.webview2 = @(
    'HKCU:\Software\Microsoft\EdgeUpdate\Clients\*',
    'HKLM:\Software\Microsoft\EdgeUpdate\Clients\*',
    'HKLM:\Software\WOW6432Node\Microsoft\EdgeUpdate\Clients\*'
) | ForEach-Object {
    Get-ItemProperty $_ -ErrorAction SilentlyContinue
} | Where-Object { $_.name -like '*WebView2*' } | Select-Object name, pv, location
$report.environmentOverrides = [ordered]@{}
foreach ($key in @('HOME', 'CC_SWITCH_TEST_HOME', 'WEBVIEW2_BROWSER_EXECUTABLE_FOLDER',
    'WEBVIEW2_USER_DATA_FOLDER')) {
    $report.environmentOverrides[$key] = [Environment]::GetEnvironmentVariable($key)
}
$report.configurationDirectories = @(
    foreach ($name in @('.cc-switch', '.osir_client')) {
        $root = Join-Path $configurationHome $name
        $entry = [ordered]@{ path = $root; exists = (Test-Path -LiteralPath $root) }
        if ($entry.exists) {
            $entry.attributes = (Get-Item -LiteralPath $root -Force).Attributes.ToString()
            $entry.database = Database-Evidence (Join-Path $root 'osir_client.db')
            $entry.legacyDatabase = Database-Evidence (Join-Path $root 'cc-switch.db')
            $entry.knownFiles = @(
                foreach ($file in @('settings.json', 'osir_client.db-wal', 'osir_client.db-shm',
                    'cc-switch.db-wal', 'cc-switch.db-shm', 'crash.log', 'startup-error.log')) {
                    $evidence = File-Evidence (Join-Path $root $file)
                    if ($evidence) { $evidence }
                }
            )
            $startupLog = Join-Path $root 'startup-error.log'
            if (Test-Path -LiteralPath $startupLog -PathType Leaf) {
                $entry.startupFailure = [ordered]@{}
                foreach ($line in @(Get-Content -LiteralPath $startupLog -Encoding UTF8 -Tail 20)) {
                    if ($line -match '^(version|os|arch|stage|category)=([A-Za-z0-9._-]{1,64})$') {
                        $entry.startupFailure[$Matches[1]] = $Matches[2]
                    }
                }
            }
            $crashLog = Join-Path $root 'crash.log'
            if (Test-Path -LiteralPath $crashLog -PathType Leaf) {
                $crashTail = @(Get-Content -LiteralPath $crashLog -Encoding UTF8 -Tail 80) -join [Environment]::NewLine
                $entry.crashFailureSignals = [ordered]@{
                    setupHookFailure = $crashTail.Contains('error encountered during setup hook')
                    configurationMigrationIncomplete = $crashTail -match '\u914d\u7f6e\u8fc1\u79fb\u672a\u5b8c\u6210'
                    webviewMentioned = $crashTail -match 'WebView2|webview runtime'
                }
            }
        }
        $entry
    }
)
$timer = [Diagnostics.Stopwatch]::StartNew()
try {
    $report.codexPackages = @(Get-AppxPackage -Name OpenAI.Codex -ErrorAction Stop |
        Select-Object Name, Version, Publisher, PackageFamilyName, InstallLocation)
    $report.codexQuerySucceeded = $true
} catch { $report.codexQuerySucceeded = $false }
$timer.Stop()
$report.codexQueryMilliseconds = $timer.ElapsedMilliseconds
$report.cliCommands = @(Get-Command claude, codex, opencode, gemini -All -ErrorAction SilentlyContinue |
    Select-Object Name, CommandType, Source)
$report.recoveryArchives = @(Get-ChildItem -LiteralPath $configurationHome -Directory -Force |
    Where-Object { $_.Name -like '.osir_client-startup-recovery-*' } |
    Select-Object Name, LastWriteTime)
$destination = $ReportPath
if (-not $destination) {
    $destination = Join-Path ([Environment]::GetFolderPath('Desktop')) (
        'OSIR-Windows-Check-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.json')
}
$json = $report | ConvertTo-Json -Depth 10
[IO.File]::WriteAllText($destination, $json, (New-Object Text.UTF8Encoding($false)))
Write-Output $json
Write-Output ('Report saved: ' + $destination)
