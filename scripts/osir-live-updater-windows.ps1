param([Parameter(Mandatory=$true)][string]$ExpectedVersion,[Parameter(Mandatory=$true)][string]$ExpectedExecutableSha256)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or -not $env:RUNNER_TEMP) { throw 'Ephemeral Windows runner required' }
$root=Join-Path $env:RUNNER_TEMP ('osir-real-update-'+[guid]::NewGuid().ToString('N'))
$install=Join-Path $root '应用目录'
$out=Join-Path $env:GITHUB_WORKSPACE 'updater-evidence'
New-Item -ItemType Directory $root,$out | Out-Null
$db=Join-Path $env:USERPROFILE '.osir_client/osir_client.db'
if(Test-Path $db){throw 'Refusing to reuse existing application data'}
if(Get-Process osir_client -ErrorAction SilentlyContinue){throw 'Preexisting OSIR process'}
$feed=Invoke-RestMethod 'https://osirclaw.com/api/v1/osir-client/updater/prod.json'
if($feed.version -ne $ExpectedVersion){throw 'Published update differs from expected release'}
$baseline=Join-Path $root 'baseline.exe'
& curl.exe --fail --max-time 240 --output $baseline 'https://osirclaw.com/api/v1/osir-client/download/prod-0.1.4/windows-x86_64/installer'
if($LASTEXITCODE -ne 0 -or (Get-FileHash $baseline -Algorithm SHA256).Hash.ToLowerInvariant() -ne '636ffcc3f2ca4f4d078cdf64c25fbbe9b23a4fdf87e02da294a7d0627bb5692a'){throw 'Baseline installer verification failed'}
$p=Start-Process $baseline -ArgumentList ('/S /D='+$install) -Wait -PassThru
if($p.ExitCode -ne 0){throw 'Baseline installation failed'}
$exe=Join-Path $install 'osir_client.exe'
if((Get-FileHash $exe -Algorithm SHA256).Hash.ToLowerInvariant() -ne 'fc7e6560147975b6f4e1fcf6e998b14e0b4f17640b09a533ba95c1df07458998'){throw 'Baseline executable verification failed'}
$listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0);$listener.Start();$port=$listener.LocalEndpoint.Port;$listener.Stop()
$key=[Microsoft.Win32.Registry]::LocalMachine.CreateSubKey('SOFTWARE\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments')
$owned=@()
try{
 foreach($id in @('osir_client.exe','com.osir.client')){if($null -ne $key.GetValue($id)){throw 'Existing debug policy'};$key.SetValue($id,"--remote-debugging-port=$port --remote-debugging-address=127.0.0.1");$owned+=$id}
 $p=Start-Process $exe -PassThru
 & node "$PSScriptRoot/osir-live-updater-check.mjs" $port prepare $out '0.1.4'
 if($LASTEXITCODE -ne 0){throw 'Baseline renderer failed'}
 Stop-Process -Id $p.Id
 $p.WaitForExit()
 @'
import sqlite3,sys
with sqlite3.connect(sys.argv[1]) as db:
 assert db.execute('pragma quick_check').fetchone()==('ok',)
 db.execute('create table osir_updater_acceptance(value text not null)')
 db.execute("insert into osir_updater_acceptance values('014-to-015-preserved')")
 db.commit()
'@ | Set-Content (Join-Path $root 'prepare.py') -Encoding utf8NoBOM
 & python (Join-Path $root 'prepare.py') $db
 if($LASTEXITCODE -ne 0){throw 'Fixture preparation failed'}
 $p=Start-Process $exe -PassThru
 & node "$PSScriptRoot/osir-live-updater-check.mjs" $port update $out '0.1.4'
 if($LASTEXITCODE -ne 0){throw 'Real update click failed'}
 $deadline=[DateTime]::UtcNow.AddMinutes(10)
 $next=$null
 while([DateTime]::UtcNow -lt $deadline){
  $next=@(Get-Process osir_client -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe -and $_.Id -ne $p.Id }) | Select-Object -First 1
  if($next -and (Get-FileHash $exe -Algorithm SHA256).Hash.ToLowerInvariant() -eq $ExpectedExecutableSha256){break}
  Start-Sleep -Seconds 1
 }
 if(-not $next){throw 'Updater did not automatically launch the new application'}
 if(-not $p.HasExited){throw 'Previous process did not exit'}
 if((Get-FileHash $exe -Algorithm SHA256).Hash.ToLowerInvariant() -ne $ExpectedExecutableSha256){throw 'Installed bytes differ'}
 & node "$PSScriptRoot/osir-live-updater-check.mjs" $port verify $out $ExpectedVersion
 if($LASTEXITCODE -ne 0){throw 'Updated renderer failed'}
 $next.Refresh()
 if($next.MainWindowHandle -eq 0 -or $next.MainWindowTitle -ne 'OSIR Client'){throw 'Updated application has no visible product window'}
 @'
import sqlite3,sys
with sqlite3.connect(sys.argv[1]) as db:
 assert db.execute('pragma quick_check').fetchone()==('ok',)
 assert db.execute('select value from osir_updater_acceptance').fetchall()==[('014-to-015-preserved',)]
'@ | Set-Content (Join-Path $root 'verify.py') -Encoding utf8NoBOM
 & python (Join-Path $root 'verify.py') $db
 if($LASTEXITCODE -ne 0){throw 'Updated data not preserved'}
 @{passed=$true;fromVersion='0.1.4';toVersion=$ExpectedVersion;oldPid=$p.Id;newPid=$next.Id;installedSha256=$ExpectedExecutableSha256;realUiClick=$true;automaticRestart=$true;syntheticDataPreserved=$true;accountAuthorizationCovered=$false;scope='ephemeral-Windows-runner-published-updater'} | ConvertTo-Json | Set-Content (Join-Path $out 'result.json') -Encoding utf8NoBOM
}finally{
 foreach($id in $owned){$key.DeleteValue($id,$false)};$key.Dispose()
 Get-Process osir_client -ErrorAction SilentlyContinue | Where-Object {$_.Path -eq $exe} | Stop-Process
}
