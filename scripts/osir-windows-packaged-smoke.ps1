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
$expectedWindowTitle = if ($manifest.channel -eq 'beta') { 'OSIR Client Beta' } else { 'OSIR Client' }
$installer = Join-Path $artifactDir $manifest.installerArtifact.name
$expectedMain = Join-Path $artifactDir $manifest.mainExecutableArtifact.name
$smokeRoot = Join-Path $env:RUNNER_TEMP ('OSIR 安装验收 ' + [guid]::NewGuid().ToString('N'))
$installDir = Join-Path $smokeRoot '应用目录'
$isolatedToolHome = Join-Path $smokeRoot '用户数据'
$isolatedAppData = Join-Path $smokeRoot 'roaming'
$isolatedLocalAppData = Join-Path $smokeRoot 'local'
$mainExe = Join-Path $installDir 'osir_client.exe'
$uninstaller = Join-Path $installDir 'uninstall.exe'
$ownedLaunch = $null
$stage = 'preflight'
$failure = $null
$rendererPort = 0
$usedRendererPorts = [Collections.Generic.HashSet[int]]::new()
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
    unicodeAndSpacePaths = $true
    authenticode = 'unsigned'
    stages = [ordered]@{}
}

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class OsirRealWindow {
    public long Handle; public string Title; public string ClassName;
    public int Left; public int Top; public int Width; public int Height;
}
public class OsirNativeControl {
    public long Handle; public int Id; public string ClassName; public string Text;
    public bool Visible; public bool Enabled;
}
public static class OsirWindows {
    private delegate bool Callback(IntPtr h, IntPtr p);
    [StructLayout(LayoutKind.Sequential)] private struct Rect { public int Left,Top,Right,Bottom; }
    [DllImport("user32.dll")] private static extern bool EnumWindows(Callback cb, IntPtr p);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] private static extern bool IsWindowEnabled(IntPtr h);
    [DllImport("user32.dll")] private static extern bool EnumChildWindows(IntPtr parent, Callback cb, IntPtr p);
    [DllImport("user32.dll")] private static extern int GetDlgCtrlID(IntPtr h);
    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern bool PostMessageW(IntPtr h, uint message, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] private static extern IntPtr SendMessageTimeoutW(IntPtr h, uint message, IntPtr w, IntPtr l, uint flags, uint timeout, out IntPtr result);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetWindowTextW(IntPtr h, StringBuilder s, int count);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetClassNameW(IntPtr h, StringBuilder s, int count);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr h, out Rect rect);
    [DllImport("user32.dll")] private static extern int GetWindowLongW(IntPtr h, int index);
    public static OsirNativeControl[] Controls(long parent) {
        var result = new List<OsirNativeControl>();
        EnumChildWindows(new IntPtr(parent), (h,p) => {
            var text=new StringBuilder(4096); GetWindowTextW(h,text,text.Capacity);
            var cls=new StringBuilder(512); GetClassNameW(h,cls,cls.Capacity);
            result.Add(new OsirNativeControl {Handle=h.ToInt64(),Id=GetDlgCtrlID(h),ClassName=cls.ToString(),Text=text.ToString(),Visible=IsWindowVisible(h),Enabled=IsWindowEnabled(h)});
            return true;
        },IntPtr.Zero);
        return result.ToArray();
    }
    public static bool ClickPrimary(long parent, int pid) {
        uint owner; GetWindowThreadProcessId(new IntPtr(parent), out owner);
        if(owner!=(uint)pid) return false;
        foreach(var control in Controls(parent)) {
            if(control.Id!=1 || control.ClassName!="Button" || !control.Visible || !control.Enabled) continue;
            IntPtr result;
            return SendMessageTimeoutW(new IntPtr(control.Handle),0x00F5,IntPtr.Zero,IntPtr.Zero,2,5000,out result)!=IntPtr.Zero;
        }
        return false;
    }
    public static bool Activate(long parent, int pid) {
        var h=new IntPtr(parent);
        uint owner; GetWindowThreadProcessId(h,out owner);
        if(owner!=(uint)pid || !IsWindowVisible(h)) return false;
        SetForegroundWindow(h);
        return GetForegroundWindow()==h;
    }
    public static bool QueuePrimary(long parent, int pid) {
        uint owner; GetWindowThreadProcessId(new IntPtr(parent),out owner);
        if(owner!=(uint)pid) return false;
        foreach(var control in Controls(parent)) {
            if(control.Id==1 && control.ClassName=="Button" && control.Visible && control.Enabled)
                return PostMessageW(new IntPtr(control.Handle),0x00F5,IntPtr.Zero,IntPtr.Zero);
        }
        return false;
    }
    public static bool QueueButton(long parent, int pid, long button) {
        uint owner; GetWindowThreadProcessId(new IntPtr(parent),out owner);
        if(owner!=(uint)pid) return false;
        foreach(var control in Controls(parent)) {
            if(control.Handle==button && control.ClassName=="Button" && control.Visible && control.Enabled)
                return PostMessageW(new IntPtr(button),0x00F5,IntPtr.Zero,IntPtr.Zero);
        }
        return false;
    }
    public static OsirRealWindow[] ForProcess(int pid) {
        var result = new List<OsirRealWindow>();
        EnumWindows((h,p) => {
            uint owner; GetWindowThreadProcessId(h, out owner);
            if(owner!=(uint)pid || !IsWindowVisible(h)) return true;
            var title=new StringBuilder(1024); GetWindowTextW(h,title,title.Capacity);
            var cls=new StringBuilder(512); GetClassNameW(h,cls,cls.Capacity);
            Rect r; if(!GetWindowRect(h,out r)) return true;
            if(title.ToString().EndsWith("-siw") || (GetWindowLongW(h,-20)&0x80)!=0) return true;
            result.Add(new OsirRealWindow {Handle=h.ToInt64(),Title=title.ToString(),ClassName=cls.ToString(),Left=r.Left,Top=r.Top,Width=r.Right-r.Left,Height=r.Bottom-r.Top});
            return true;
        },IntPtr.Zero);
        return result.ToArray();
    }
}
'@

function Get-RealWindow([int]$ProcessId) {
    return @([OsirWindows]::ForProcess($ProcessId) | Where-Object { $_.Title -eq $expectedWindowTitle -and $_.Width -ge 900 -and $_.Height -ge 600 }) | Select-Object -First 1
}

function Assert-Smoke([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "[$script:stage] $Message" }
}

function Get-Hash([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Start-Isolated([string]$Path, [string]$Arguments = '', [bool]$MissingRuntime = $false) {
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Path
    $start.Arguments = $Arguments
    $start.WorkingDirectory = $smokeRoot
    $start.UseShellExecute = $false
    $start.Environment['CC_SWITCH_TEST_HOME'] = $isolatedToolHome
    $start.Environment['OSIR_TEST_HOME'] = $isolatedToolHome
    $start.Environment['APPDATA'] = $isolatedAppData
    $start.Environment['LOCALAPPDATA'] = $isolatedLocalAppData
    if ($Path -eq $mainExe -and -not $MissingRuntime) {
        do {
            $debugListener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
            $debugListener.Start()
            $script:rendererPort = $debugListener.LocalEndpoint.Port
            $debugListener.Stop()
        } while (-not $script:usedRendererPorts.Add($script:rendererPort))
        $start.Environment['WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS'] = "--remote-debugging-port=$script:rendererPort --remote-debugging-address=127.0.0.1"
    }
    if ($MissingRuntime) {
        $start.Environment['WEBVIEW2_BROWSER_EXECUTABLE_FOLDER'] = Join-Path $smokeRoot 'missing-webview2-runtime'
    }
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

function Get-WizardPageSignature($Element) {
    $nodes = $Element.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    $values = foreach ($node in $nodes) {
        if (-not $node.Current.IsOffscreen) {
            "$($node.Current.ControlType.ProgrammaticName):$($node.Current.AutomationId):$($node.Current.Name)"
        }
    }
    return ($values -join "`n")
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
    $stage = 'installerOpen'
    $interactiveInstaller = Join-Path $smokeRoot 'OSIR Client 安装程序.exe'
    Copy-Item -LiteralPath $installer -Destination $interactiveInstaller
    Assert-Smoke ((Get-Hash $interactiveInstaller) -eq $manifest.installerArtifact.sha256) 'Interactive installer copy differs from the verified payload.'
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $ownedLaunch = Start-Isolated $interactiveInstaller
    $deadline = [DateTime]::UtcNow.AddSeconds($LaunchSeconds)
    $wizard = $null
    while ([DateTime]::UtcNow -lt $deadline) {
        $ownedLaunch.Refresh()
        Assert-Smoke (-not $ownedLaunch.HasExited) 'Installer exited before showing its setup wizard.'
        $wizard = @([OsirWindows]::ForProcess($ownedLaunch.Id) | Where-Object {
            $_.ClassName -eq '#32770' -and $_.Title.Contains($expectedWindowTitle) -and $_.Width -gt 200 -and $_.Height -gt 150
        }) | Select-Object -First 1
        if ($null -ne $wizard) { break }
        Start-Sleep -Milliseconds 250
    }
    Assert-Smoke ($null -ne $wizard) 'Opening the installer did not display a setup wizard.'
    $timer.Stop()
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $nextCondition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, '1')
    $navigationDeadline = [DateTime]::UtcNow.AddSeconds($LaunchSeconds)
    $enabledButtons = 0
    $nativeNext = $null
    # Some runner UIA providers expose Win32 buttons as Pane. Verify their real
    # HWND class and state, then require an actual page transition after clicking.
    while ([DateTime]::UtcNow -lt $navigationDeadline) {
        $ownedLaunch.Refresh()
        Assert-Smoke (-not $ownedLaunch.HasExited) 'Installer exited while preparing its setup wizard.'
        $wizard = @([OsirWindows]::ForProcess($ownedLaunch.Id) | Where-Object {
            $_.ClassName -eq '#32770' -and $_.Title.Contains($expectedWindowTitle) -and $_.Width -gt 200 -and $_.Height -gt 150
        }) | Select-Object -First 1
        try {
            if ($null -ne $wizard) {
                $wizardElement = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$wizard.Handle)
                $nativeButtons = @([OsirWindows]::Controls($wizard.Handle) | Where-Object { $_.ClassName -eq 'Button' -and $_.Visible -and $_.Enabled })
                $enabledButtons = $nativeButtons.Count
                $nativeNext = $nativeButtons | Where-Object { $_.Id -eq 1 } | Select-Object -First 1
                if ($enabledButtons -ge 2 -and $null -ne $nativeNext) { break }
            }
        } catch { $nativeNext = $null }
        Start-Sleep -Milliseconds 250
    }
    Add-Type -AssemblyName System.Drawing
    Assert-Smoke ($null -ne $wizard) 'Installer setup wizard disappeared.'
    $bitmap = [Drawing.Bitmap]::new($wizard.Width, $wizard.Height)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($wizard.Left, $wizard.Top, 0, 0, $bitmap.Size)
        $bitmap.Save((Join-Path $artifactDir 'windows-installer-window.png'), [Drawing.Imaging.ImageFormat]::Png)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
    Get-WizardPageSignature $wizardElement | Set-Content -LiteralPath (Join-Path $artifactDir 'windows-installer-controls.txt') -Encoding utf8NoBOM
    $nativeControls = [OsirWindows]::Controls($wizard.Handle) | ConvertTo-Json -Depth 4
    $nativeControls | Set-Content -LiteralPath (Join-Path $artifactDir 'windows-installer-native-controls.json') -Encoding utf8NoBOM
    Write-Host (Get-WizardPageSignature $wizardElement)
    Write-Host $nativeControls
    Assert-Smoke ($enabledButtons -ge 2) 'Installer wizard has no usable navigation buttons after waiting for initialization.'
    Assert-Smoke ($null -ne $nativeNext) 'Installer Next action is unavailable.'
    $beforePage = Get-WizardPageSignature $wizardElement
    Assert-Smoke ([OsirWindows]::Activate($wizard.Handle, $ownedLaunch.Id)) 'Installer setup wizard could not become active.'
    Assert-Smoke ([OsirWindows]::ClickPrimary($wizard.Handle, $ownedLaunch.Id)) 'Installer Next button could not be clicked.'
    $pageChanged = $false
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while ([DateTime]::UtcNow -lt $deadline) {
        try { $pageChanged = (Get-WizardPageSignature $wizardElement) -ne $beforePage } catch { $pageChanged = $false }
        if ($pageChanged) { break }
        Start-Sleep -Milliseconds 200
    }
    Assert-Smoke $pageChanged 'Installer Next action did not advance the wizard.'
    $report.stages.installerOpen = [ordered]@{
        passed = $true; windowObserved = $true; usableNavigation = $true
        nextActionInvoked = $true; nextPageObserved = $true
        navigationObservedWith = 'owned-process-Win32-Button-class-id-visible-enabled'; pageTransitionObservedWith = 'UIAutomation-content'
        elapsedMilliseconds = $timer.ElapsedMilliseconds
        execution = 'non-silent-no-arguments'; trustScope = 'CI-local-file-without-internet-zone'
    }
    Stop-OwnedApp

    $stage = 'install'
    # NSIS requires /D as the last raw argument, without quotation marks even
    # when the destination contains spaces. It consumes the remaining command.
    Invoke-Installer $installer ("/S /D=" + $installDir)
    $report.stages.install = Assert-Installed

    $stage = 'launch'
    $ownedLaunch = Start-Isolated $mainExe
    $deadline = [DateTime]::UtcNow.AddSeconds($LaunchSeconds)
    $realWindow = $null
    while ([DateTime]::UtcNow -lt $deadline) {
        $ownedLaunch.Refresh()
        Assert-Smoke (-not $ownedLaunch.HasExited) 'Installed application exited before displaying its window.'
        $realWindow = Get-RealWindow $ownedLaunch.Id
        if ($null -ne $realWindow) { break }
        Start-Sleep -Milliseconds 250
    }
    if ($null -eq $realWindow) {
        $ownedLaunch.Refresh()
        $processSnapshot = Get-Process -Id $ownedLaunch.Id -ErrorAction SilentlyContinue
        $windowSnapshot = @([OsirWindows]::ForProcess($ownedLaunch.Id) | ForEach-Object {
            [ordered]@{
                title = $_.Title
                className = $_.ClassName
                width = $_.Width
                height = $_.Height
            }
        })
        $childSnapshot = @()
        try {
            $childSnapshot = @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $($ownedLaunch.Id)" |
                Select-Object ProcessId, Name, ParentProcessId)
        } catch { }
        $logFiles = @()
        $currentLogDir = Join-Path $isolatedToolHome '.osir_client/logs'
        if (Test-Path -LiteralPath $currentLogDir -PathType Container) {
            $logFiles = @(Get-ChildItem -LiteralPath $currentLogDir -File -ErrorAction SilentlyContinue |
                Select-Object Name, Length, LastWriteTimeUtc)
        }
        $report.launchDiagnostics = [ordered]@{
            processId = $ownedLaunch.Id
            processName = if ($null -ne $processSnapshot) { $processSnapshot.ProcessName } else { $null }
            executablePath = if ($null -ne $processSnapshot) { $processSnapshot.Path } else { $null }
            mainWindowTitle = if ($null -ne $processSnapshot) { $processSnapshot.MainWindowTitle } else { $null }
            hasExited = $ownedLaunch.HasExited
            exitCode = if ($ownedLaunch.HasExited) { $ownedLaunch.ExitCode } else { $null }
            windows = $windowSnapshot
            childProcesses = $childSnapshot
            currentLogFiles = $logFiles
        }
        $report.launchDiagnostics | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $artifactDir 'windows-launch-diagnostics.json') -Encoding utf8NoBOM
        Write-Host ($report.launchDiagnostics | ConvertTo-Json -Depth 8)
    }
    Assert-Smoke ($null -ne $realWindow) 'Installed application did not display a visible OSIR Client window of at least 900x600.'
    # Observe briefly after the window appears; a momentary handle is insufficient.
    Start-Sleep -Seconds 3
    $ownedLaunch.Refresh()
    $realWindow = Get-RealWindow $ownedLaunch.Id
    Assert-Smoke (-not $ownedLaunch.HasExited -and $null -ne $realWindow) 'Application window closed immediately after launch.'
    # A visible native shell can still contain a blank or unusable WebView.
    & node (Join-Path $PSScriptRoot 'osir-packaged-renderer-check.mjs') $rendererPort $artifactDir 'launch' $realWindow.Handle
    Assert-Smoke ($LASTEXITCODE -eq 0) 'Installed renderer or native IPC failed.'
    $rendererReport = Get-Content -Raw -LiteralPath (Join-Path $artifactDir 'windows-renderer-smoke.json') | ConvertFrom-Json
    Assert-Smoke ($rendererReport.passed -eq $true) 'Installed renderer did not pass.'
    $realWindow = Get-RealWindow $ownedLaunch.Id
    Assert-Smoke ($null -ne $realWindow) 'Native product window disappeared during renderer acceptance.'
    Add-Type -AssemblyName System.Drawing
    $bitmap = [Drawing.Bitmap]::new($realWindow.Width, $realWindow.Height)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($realWindow.Left, $realWindow.Top, 0, 0, $bitmap.Size)
        $bitmap.Save((Join-Path $artifactDir 'windows-app-window.png'), [Drawing.Imaging.ImageFormat]::Png)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
    $report.stages.launch = [ordered]@{
        passed = $true
        processId = $ownedLaunch.Id
        windowObserved = $true
        windowTitle = $realWindow.Title
        windowClass = $realWindow.ClassName
        windowWidth = $realWindow.Width
        windowHeight = $realWindow.Height
        excludesSingleInstanceToolWindow = $true
        remainedRunning = $true
        rendererAndNativeIpcPassed = $true
    }
    Stop-OwnedApp

    $stage = 'reopen'
    $ownedLaunch = Start-Isolated $mainExe
    $deadline = [DateTime]::UtcNow.AddSeconds($LaunchSeconds)
    $realWindow = $null
    while ([DateTime]::UtcNow -lt $deadline) {
        $ownedLaunch.Refresh()
        Assert-Smoke (-not $ownedLaunch.HasExited) 'Installed application exited on reopen.'
        $realWindow = Get-RealWindow $ownedLaunch.Id
        if ($null -ne $realWindow) { break }
        Start-Sleep -Milliseconds 250
    }
    Assert-Smoke ($null -ne $realWindow) 'Installed application did not reopen its product window.'
    & node (Join-Path $PSScriptRoot 'osir-packaged-renderer-check.mjs') $rendererPort $artifactDir 'reopen' $realWindow.Handle
    Assert-Smoke ($LASTEXITCODE -eq 0) 'Reopened renderer or native IPC failed.'
    $report.stages.reopen = [ordered]@{ passed = $true; windowObserved = $true; rendererAndNativeIpcPassed = $true }
    Stop-OwnedApp

    $stage = 'startupFailure'
    $ownedLaunch = Start-Isolated $mainExe '' $true
    $deadline = [DateTime]::UtcNow.AddSeconds($LaunchSeconds)
    $errorWindow = $null
    while ([DateTime]::UtcNow -lt $deadline) {
        $ownedLaunch.Refresh()
        Assert-Smoke (-not $ownedLaunch.HasExited) 'Startup failure exited without a visible explanation.'
        $failureWindows = @([OsirWindows]::ForProcess($ownedLaunch.Id))
        $errorWindow = $failureWindows | Where-Object { $_.Title -eq 'OSIR Client 无法启动' } | Select-Object -First 1
        if ($null -ne $errorWindow) { break }
        Start-Sleep -Milliseconds 250
    }
    if ($null -eq $errorWindow) {
        Write-Host ($failureWindows | ConvertTo-Json -Depth 4)
        foreach ($window in $failureWindows) {
            $element = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$window.Handle)
            Write-Host (Get-WizardPageSignature $element)
        }
    }
    Assert-Smoke ($null -ne $errorWindow) 'Missing runtime did not display a native startup error.'
    $errorElement = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$errorWindow.Handle)
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        $ownedLaunch.Refresh()
        Assert-Smoke (-not $ownedLaunch.HasExited) 'Startup error exited before confirmation.'
        $errorText = Get-WizardPageSignature $errorElement
        $errorControls = @([OsirWindows]::Controls($errorWindow.Handle))
        $nativeErrorText = ($errorControls | Where-Object { $_.Visible } | ForEach-Object { $_.Text }) -join "`n"
        $confirmationButtons = @($errorControls | Where-Object { $_.ClassName -eq 'Button' -and $_.Visible -and $_.Enabled })
        $ok = if ($confirmationButtons.Count -eq 1) { $confirmationButtons[0] } else { $null }
        $recoveryReady = $nativeErrorText.Contains('安装程序') -and $nativeErrorText.Contains('日志') -and $null -ne $ok
        if ($recoveryReady) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    $errorText | Set-Content -LiteralPath (Join-Path $artifactDir 'windows-startup-error-controls.txt') -Encoding utf8NoBOM
    $errorControls | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $artifactDir 'windows-startup-error-native-controls.json') -Encoding utf8NoBOM
    Assert-Smoke $recoveryReady 'Startup error does not provide a visible recovery action and log location.'
    $startupError = Join-Path $isolatedToolHome '.osir_client/startup-error.log'
    Assert-Smoke (Test-Path -LiteralPath $startupError -PathType Leaf) 'Startup failure diagnostic is missing.'
    $diagnostic = Get-Content -Raw -LiteralPath $startupError
    Assert-Smoke ($diagnostic -match 'stage=webview-runtime' -and $diagnostic -match 'category=webview-runtime') 'Startup diagnostic is incomplete.'
    $errorText | Set-Content -LiteralPath (Join-Path $artifactDir 'windows-startup-error-controls.txt') -Encoding utf8NoBOM
    $bitmap = [Drawing.Bitmap]::new($errorWindow.Width, $errorWindow.Height)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($errorWindow.Left, $errorWindow.Top, 0, 0, $bitmap.Size)
        $bitmap.Save((Join-Path $artifactDir 'windows-startup-error-window.png'), [Drawing.Imaging.ImageFormat]::Png)
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
    Assert-Smoke ([OsirWindows]::Activate($errorWindow.Handle, $ownedLaunch.Id)) 'Startup error could not become active.'
    Assert-Smoke ([OsirWindows]::QueueButton($errorWindow.Handle, $ownedLaunch.Id, $ok.Handle)) 'Startup error confirmation could not be clicked.'
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while ([DateTime]::UtcNow -lt $deadline -and -not $ownedLaunch.HasExited) {
        $ownedLaunch.Refresh()
        $remaining = @([OsirWindows]::ForProcess($ownedLaunch.Id))
        Assert-Smoke (-not ($remaining | Where-Object { $_.Title -eq 'Error' -or $_.Title -eq $expectedWindowTitle })) 'Dismissed startup error continued into runtime or product UI.'
        Start-Sleep -Milliseconds 100
    }
    Assert-Smoke ($ownedLaunch.WaitForExit(1000) -and $ownedLaunch.ExitCode -eq 1) 'Startup error confirmation did not terminate with exit code 1.'
    $report.stages.startupFailure = [ordered]@{ passed = $true; nativeErrorVisible = $true; recoveryMessageVisible = $true; diagnosticSaved = $true; preflightRuntimeCheck = $true; confirmationInvoked = $true; exitCode = 1; kind = 'forced-unavailable-runtime' }
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
    $startupLogDir = Join-Path $isolatedToolHome '.osir_client/logs'
    if (Test-Path -LiteralPath $startupLogDir -PathType Container) {
        Copy-Item -LiteralPath $startupLogDir -Destination (Join-Path $artifactDir 'windows-startup-logs') -Recurse -Force -ErrorAction Continue
    }
    $startupError = Join-Path $isolatedToolHome '.osir_client/startup-error.log'
    if (Test-Path -LiteralPath $startupError -PathType Leaf) {
        Copy-Item -LiteralPath $startupError -Destination (Join-Path $artifactDir 'windows-startup-error.log') -Force -ErrorAction Continue
    }
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
Write-Host 'Packaged lifecycle passed: install, real renderer/native IPC, reopen, UPDATE reinstall, uninstall.'
