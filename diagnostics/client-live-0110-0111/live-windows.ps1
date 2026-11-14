param([Parameter(Mandatory=$true)][string]$Spec,[Parameter(Mandatory=$true)][string]$Workspace,[Parameter(Mandatory=$true)][string]$Platform)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if(-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:CI -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' -or $env:GITHUB_REPOSITORY -ne 'ousir0/osir-client'){throw 'Disposable authorized Windows runner required'}
if($Platform -ne 'windows-x86_64' -or [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture -ne 'X64' -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64'){throw 'Native Windows x64 required'}
$root=[IO.Path]::GetFullPath($Workspace)
$tempRoot=[IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\')+'\'
if(-not $root.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase)){throw 'Workspace escaped RUNNER_TEMP'}
$marker=Get-Content -LiteralPath (Join-Path $root 'osir-updater-0110-to-0111-owned.json') -Raw | ConvertFrom-Json
if($marker.runId -ne $env:GITHUB_RUN_ID -or $marker.purpose -ne '0110-to-0111-synthetic-only'){throw 'Run ownership mismatch'}
$native=Get-Content -LiteralPath (Join-Path $root 'runtime-input.json') -Raw | ConvertFrom-Json
if($native.platform -ne $Platform){throw 'Prepared platform mismatch'}
$out=Join-Path $root 'evidence'
$events=[Collections.Generic.List[object]]::new()
$ownedPolicies=[Collections.Generic.List[string]]::new()
$key=$null;$app=$null;$next=$null;$installer=$null;$exe=$null;$clicked=$false;$stage='gui-preflight'
function Event([string]$Phase){$events.Add(@{phase=$Phase;at=[DateTime]::UtcNow.ToString('o')})}
function Save([string]$Path,$Value){if(Test-Path -LiteralPath $Path){throw 'Evidence already exists'};$Value | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $Path -Encoding utf8NoBOM}
function Hash([string]$Path){(Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()}
function Stop-Verified($Process,[string]$Expected){
 if($null -eq $Process){return}
 $Process.Refresh();if($Process.HasExited){return}
 $current=Get-Process -Id $Process.Id -ErrorAction Stop
 if($current.Path -ne $Expected -or $current.StartTime -ne $Process.StartTime){throw 'Owned PID identity changed; signal refused'}
 Stop-Process -Id $Process.Id -ErrorAction Stop
 if(-not $Process.WaitForExit(20000)){throw 'Owned process did not exit'}
}
function Wait-Schema($Process){
 for($i=0;$i -lt 90;$i++){
  $Process.Refresh();if($Process.HasExited){throw 'Old process exited before schema initialization'}
  & python "$PSScriptRoot/helpers/wait-schema.py" $root
  if($LASTEXITCODE -eq 0){return}
  Start-Sleep -Seconds 1
 }
 throw 'Actual old app did not initialize schema 26'
}
function Launch-Old {
 if($script:clicked){throw 'Runtime launch after update click refused'}
 if((Hash $script:exe) -ne $native.old.executableSha256){throw 'Baseline bytes changed before controlled launch'}
 Start-Process -FilePath $script:exe -PassThru -RedirectStandardOutput (Join-Path $out ('app-'+[guid]::NewGuid().ToString('N')+'.stdout.log')) -RedirectStandardError (Join-Path $out ('app-'+[guid]::NewGuid().ToString('N')+'.stderr.log'))
}
function Assert-DebugOwner($Process,[int]$Port){
 $deadline=[DateTime]::UtcNow.AddSeconds(90)
 while([DateTime]::UtcNow -lt $deadline){
  $rows=@(Get-NetTCPConnection -State Listen -LocalPort $Port -ErrorAction SilentlyContinue)
  if($rows.Count -eq 0){Start-Sleep -Milliseconds 500;continue}
  foreach($row in $rows){
   if($row.LocalAddress -notin @('127.0.0.1','::1')){throw 'Debug listener is not loopback'}
   $cursor=[int]$row.OwningProcess;$seen=@{}
   while($cursor -ne $Process.Id -and $cursor -gt 0 -and -not $seen.ContainsKey($cursor)){
    $seen[$cursor]=$true
    $ancestor=Get-CimInstance Win32_Process -Filter "ProcessId = $cursor"
    if($null -eq $ancestor){break};$cursor=[int]$ancestor.ParentProcessId
   }
   if($cursor -ne $Process.Id){throw 'CDP listener does not belong to owned native app tree'}
  }
  Save (Join-Path $out ('debug-owner-'+$Process.Id+'.json')) @{nativePid=$Process.Id;port=$Port;listenerPids=@($rows | ForEach-Object {$_.OwningProcess});loopback=$true;descendantOfOwnedNativePid=$true}
  return
 }
 throw 'BLOCKED: native WebView debug listener unavailable; native GUI result not inferred'
}
try{
 if(-not [Environment]::UserInteractive){throw 'BLOCKED: interactive Windows desktop unavailable'}
 Add-Type -Path "$PSScriptRoot/helpers/windows-window.cs"
 if(Get-Process osir_client -ErrorAction SilentlyContinue){throw 'Preexisting OSIR process refused'}
 $nativeHome=[Environment]::GetFolderPath('UserProfile')
 if($nativeHome -ne $env:USERPROFILE -or (Get-Item -LiteralPath $nativeHome).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)){throw 'Native runner profile identity differs'}
 foreach($name in @('.osir_client','.cc-switch','.codex','.claude')){
  if(Test-Path -LiteralPath (Join-Path $nativeHome $name)){throw 'Preexisting default app/tool data refused'}
 }
 # NSIS RunAsUser relaunch does not promise process-only environment overrides.
 # A clean hosted profile keeps old/new data paths identical without such overrides.
 Save (Join-Path $root 'private/windows-profile.json') @{runId=$env:GITHUB_RUN_ID;purpose='0110-to-0111-synthetic-only';profile=$nativeHome}
 Save (Join-Path $out 'windows-profile-binding.json') @{nativeProfile=$nativeHome;initiallyAbsent=@('.osir_client','.cc-switch','.codex','.claude');syntheticOnly=$true}
 foreach($hive in @('HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall','HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall','HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall')){
  if(Test-Path $hive){foreach($entry in Get-ChildItem $hive){$v=Get-ItemProperty $entry.PSPath;if($v.PSObject.Properties['DisplayName'] -and $v.DisplayName -eq 'OSIR Client'){throw 'Preexisting registered Client refused'}}}
 }
 $stage='baseline-install'
 $install=Join-Path $root 'install/应用目录'
 $installer=Start-Process -FilePath $native.baseline -ArgumentList ('/S /D='+$install) -PassThru
 if(-not $installer.WaitForExit(120000)){throw 'Baseline installer timed out'}
 if($installer.ExitCode -ne 0){throw 'Actual baseline installation failed'}
 $exe=Join-Path $install 'osir_client.exe'
 if((Hash $exe) -ne $native.old.executableSha256){throw 'Actual old installed bytes differ'}
 & python "$PSScriptRoot/helpers/pe-arch.py" $exe
 if($LASTEXITCODE -ne 0){throw 'Actual baseline architecture differs'}
 if(Get-Process osir_client -ErrorAction SilentlyContinue){throw 'Unexpected runtime after silent baseline install'}
 $settings=Join-Path $nativeHome '.osir_client/settings.json'
 New-Item -ItemType Directory -Path (Split-Path $settings) | Out-Null
 Save $settings @{language='zh';sessionAutoSyncEnabled=$false;unifyCodexSessionHistory=$false;checkToolUpdatesOnStartup=$false;launchOnStartup=$false;enableLocalProxy=$false;enableStackMode=$false;minimizeToTrayOnClose=$false;showProfileSwitcher=$true}
 Remove-Item Env:CC_SWITCH_TEST_HOME -ErrorAction SilentlyContinue
 $env:CODEX_HOME=Join-Path $nativeHome '.codex'
 $env:CLAUDE_CONFIG_DIR=Join-Path $nativeHome '.claude'
 $env:OPENCODE_CONFIG_DIR=Join-Path $nativeHome '.config/opencode'
 $env:WEBVIEW2_USER_DATA_FOLDER=Join-Path $root 'webview-data'
 $listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0);$listener.Start();$port=$listener.LocalEndpoint.Port;$listener.Stop()
 $key=[Microsoft.Win32.Registry]::LocalMachine.CreateSubKey('SOFTWARE\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments')
 foreach($id in @('osir_client.exe','com.osir.client')){
  if($null -ne $key.GetValue($id)){throw 'Existing debug policy refused'}
  $key.SetValue($id,"--remote-debugging-port=$port --remote-debugging-address=127.0.0.1");$ownedPolicies.Add($id)
 }
 $stage='baseline-initialization'
 $app=Launch-Old
 Wait-Schema $app
 Assert-DebugOwner $app $port
 & node "$PSScriptRoot/helpers/check-ui.mjs" $port prepare $out '0.1.10'
 if($LASTEXITCODE -ne 0){throw 'Baseline real renderer/native IPC unavailable'}
 Event 'old-ready'
 Stop-Verified $app $exe
 & python "$PSScriptRoot/reviewed/disposable.py" seed $root
 if($LASTEXITCODE -ne 0){throw 'Reviewed fixture seed failed'}
 Event 'fixture-seeded'
 $app=Launch-Old;$oldPid=$app.Id
 Wait-Schema $app
 # Same remote port but a new WebView/native PID; provenance is checked again.
 Assert-DebugOwner $app $port
 Event 'old-ready-with-fixture'
 & node "$PSScriptRoot/contract.mjs" recheck-feed $Spec $root $Platform
 if($LASTEXITCODE -ne 0){throw 'Live feed changed before click'}
 $stage='actual-update-click'
 & node "$PSScriptRoot/helpers/check-ui.mjs" $port update $out '0.1.10' '0.1.11'
 if($LASTEXITCODE -ne 0){throw 'Actual update button was not clicked; installer/manual restart fallback refused'}
 $clicked=$true;Event 'update-clicked'
 # No Start-Process is permitted on the update/restart path beyond this point.
 $stage='automatic-restart'
 $deadline=[DateTime]::UtcNow.AddMinutes(20);$exitRecorded=$false
 while([DateTime]::UtcNow -lt $deadline){
  $app.Refresh()
  if($app.HasExited){
   if(-not $exitRecorded){Event 'old-exited';$exitRecorded=$true}
   $candidates=@(Get-Process osir_client -ErrorAction SilentlyContinue | Where-Object {$_.Path -eq $exe -and $_.Id -ne $oldPid})
   if($candidates.Count -gt 1){throw 'Ambiguous automatically restarted process'}
   if($candidates.Count -eq 1 -and (Hash $exe) -eq $native.executableSha256){$next=$candidates[0];Event 'new-observed';break}
  }
  Start-Sleep -Seconds 1
 }
 if($null -eq $next){throw 'Old PID exit plus automatic verified new PID were not observed'}
 & python "$PSScriptRoot/helpers/pe-arch.py" $exe
 if($LASTEXITCODE -ne 0){throw 'Actual updated architecture differs'}
 $stage='updated-native-window'
 Assert-DebugOwner $next $port
 & node "$PSScriptRoot/helpers/check-ui.mjs" $port verify $out '0.1.11'
 if($LASTEXITCODE -ne 0){throw 'Automatic new runtime failed native version/renderer verification'}
 $next.Refresh()
 $windows=@([OsirWindows]::ForProcess($next.Id) | Where-Object {$_.Title -eq 'OSIR Client' -and $_.Width -ge 900 -and $_.Height -ge 600})
 if($windows.Count -ne 1){throw 'Automatically restarted app lacks a unique visible native product window'}
 $window=$windows[0]
 Save (Join-Path $out 'updated-window.json') @{pid=$next.Id;path=$next.Path;handle=$window.Handle;title=$window.Title;visible=$true;width=$window.Width;height=$window.Height;fileVersion=(Get-Item -LiteralPath $exe).VersionInfo.ProductVersion;executableSha256=(Hash $exe)}
 Event 'new-window'
 $stage='data-retention'
 $newPid=$next.Id;Stop-Verified $next $exe
 & python "$PSScriptRoot/reviewed/disposable.py" verify $root
 if($LASTEXITCODE -ne 0){throw 'Actual DB26 to DB26 data retention failed'}
 Event 'data-retained'
 Save (Join-Path $out 'observation.json') @{platform=$Platform;fromVersion='0.1.10';toVersion='0.1.11';sourceCommit=$native.sourceCommit;sourceSha256=$native.sourceSha256;manifestSha256=$native.manifestSha256;oldPid=$oldPid;newPid=$newPid;oldPath=$exe;newPath=$exe;architecture='x86_64';oldExecutableSha256=$native.old.executableSha256;newExecutableSha256=(Hash $exe);realUiClick=$true;oldPidExited=$true;automaticRestart=$true;nativeWindow=$true;syntheticDataPreserved=$true;nativeVersionVerified=$true;signatureVerified=$true;harnessLaunchedAfterClick=$false;accountAuthorizationCovered=$false;oldSchema=26;newSchema=26;events=@($events.ToArray())}
 & node "$PSScriptRoot/contract.mjs" finalize $Spec $root $Platform
 if($LASTEXITCODE -ne 0){throw 'Final evidence contract refused incomplete acceptance'}
}catch{
 $message=$_.Exception.Message
 Save (Join-Path $out 'platform-failure.json') @{status=$(if($message.StartsWith('BLOCKED:')){'blocked'}else{'failed'});passed=$false;stage=$stage;reason=$message;events=@($events.ToArray())}
 throw
}finally{
 if($null -ne $key){foreach($id in $ownedPolicies){$key.DeleteValue($id,$false)};$key.Dispose()}
 if($null -ne $exe){Stop-Verified $app $exe;Stop-Verified $next $exe}
 if($null -ne $installer){Stop-Verified $installer $native.baseline}
}
