# Runs only on a disposable macOS CI account, never the developer's live account.
import hashlib,json,os,plistlib,sqlite3,subprocess,sys,time,urllib.request
from pathlib import Path
assert sys.platform=='darwin' and os.environ.get('GITHUB_ACTIONS')=='true'
expected, executable_sha=sys.argv[1:]
root=Path(os.environ['RUNNER_TEMP'])/'osir-live-update';root.mkdir()
out=Path(os.environ['GITHUB_WORKSPACE'])/'updater-evidence';out.mkdir()
db=Path.home()/'.osir_client/osir_client.db'
assert not db.exists(),'Preexisting app data: refusing to use it'
feed=json.load(urllib.request.urlopen('https://osirclaw.com/api/v1/osir-client/updater/prod.json'))
assert feed['version']==expected
archive=root/'old.tar.gz'
subprocess.run(['curl','--fail','--max-time','240','--output',str(archive),'https://osirclaw.com/api/v1/osir-client/download/prod-0.1.4/darwin-x86_64/updater'],check=True)
assert hashlib.sha256(archive.read_bytes()).hexdigest()=='3af56b2488dfdc6c22514256236b329a2e62a31c9c75d160b7adb036b39378ce'
subprocess.run(['tar','-xzf',str(archive),'-C',str(root)],check=True)
app=root/'OSIR Client.app';binary=app/'Contents/MacOS/osir_client'
assert hashlib.sha256(binary.read_bytes()).hexdigest()=='33df73f7d9a18b371afb140128187e2fdff3018878021ed6f24ae6ca69676243'
subprocess.run(['codesign','--verify','--deep','--strict',str(app)],check=True)
helper=root/'click'
subprocess.run(['swiftc',str(Path(__file__).with_name('osir-updater-click.swift')),'-o',str(helper)],check=True)
log=(out/'app.log').open('w')
process=subprocess.Popen([str(binary)],stdout=log,stderr=log)
try:
 for _ in range(60):
  assert process.poll() is None
  if db.exists():break
  time.sleep(1)
 assert db.exists()
 process.terminate();process.wait(15)
 with sqlite3.connect(db) as conn:
  assert conn.execute('pragma quick_check').fetchone()==('ok',)
  conn.execute('create table osir_updater_acceptance(value text not null)')
  conn.execute("insert into osir_updater_acceptance values('014-to-015-preserved')")
  conn.commit()
 process=subprocess.Popen([str(binary)],stdout=log,stderr=log)
 subprocess.run([str(helper),str(process.pid),str(app)],check=True,timeout=115)
 new_pid=None
 for _ in range(600):
  rows=subprocess.check_output(['ps','-axo','pid=,command='],text=True).splitlines()
  candidates=[int(r.strip().split(' ',1)[0]) for r in rows if r.strip().endswith(str(binary)) and int(r.strip().split(' ',1)[0])!=process.pid]
  if candidates and process.poll() is not None and hashlib.sha256(binary.read_bytes()).hexdigest()==executable_sha:
   new_pid=candidates[0];break
  time.sleep(1)
 assert new_pid is not None,'New version did not restart automatically'
 with (app/'Contents/Info.plist').open('rb') as f: assert plistlib.load(f)['CFBundleShortVersionString']==expected
 subprocess.run(['codesign','--verify','--deep','--strict',str(app)],check=True)
 with sqlite3.connect(db) as conn:
  assert conn.execute('pragma quick_check').fetchone()==('ok',)
  assert conn.execute('select value from osir_updater_acceptance').fetchall()==[('014-to-015-preserved',)]
 time.sleep(5)
 probe=root/'window-probe'
 subprocess.run(['swiftc',str(Path(__file__).with_name('osir-macos-window-probe.swift')),'-o',str(probe)],check=True)
 window=json.loads(subprocess.check_output([str(probe),str(new_pid),'com.osir.client'],text=True))
 assert window['bundlePath']==str(app) and window['windows'],'Updated application has no visible product window'
 (out/'updated-window.json').write_text(json.dumps(window,indent=2))
 subprocess.run(['screencapture','-x',str(out/'updated.png')],check=True)
 (out/'result.json').write_text(json.dumps(dict(passed=True,fromVersion='0.1.4',toVersion=expected,oldPid=process.pid,newPid=new_pid,installedSha256=executable_sha,realUiClick=True,automaticRestart=True,syntheticDataPreserved=True,accountAuthorizationCovered=False),indent=2))
finally:
 if process.poll() is None:process.terminate();process.wait(15)
 if 'new_pid' in locals() and new_pid is not None:os.kill(new_pid,15)
 log.close()
