"""Remote native live-updater acceptance; never launch on a local account."""
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import signal
import sqlite3
import subprocess
import sys
import tarfile
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / 'reviewed'))
import disposable as retention


def stamp(): return datetime.now(timezone.utc).isoformat()
def require(v, message):
    if not v: raise ValueError(message)
def digest(p): return retention.digest(p)
def run(args, **kwargs): return subprocess.check_output([str(x) for x in args], text=True, timeout=60, **kwargs).strip()
def save(p, d): retention.save(p, d)


def schema_ready(db, process):
    for _ in range(90):
        require(process.poll() is None, 'Owned baseline process exited before initialization')
        if db.is_file():
            try:
                with sqlite3.connect(db.as_uri() + '?mode=ro', uri=True) as c:
                    if c.execute('pragma user_version').fetchone() == (26,) and c.execute("select count(*) from sqlite_master where type='table' and name in ('providers','settings','profiles')").fetchone() == (3,): return
            except sqlite3.Error: pass
        time.sleep(1)
    raise ValueError('Actual baseline did not initialize schema 26')


def processes(binary):
    rows = run(['/bin/ps', '-axo', 'pid=,comm=']).splitlines()
    return [int(row.strip().split(None, 1)[0]) for row in rows if len(row.strip().split(None, 1)) == 2 and row.strip().split(None, 1)[1] == str(binary)]


def stop_owned(pid, binary, process=None):
    # Never signal by name/bundle ID or trust only a saved numeric PID.
    current = subprocess.run(['/bin/ps', '-p', str(pid), '-o', 'comm='], capture_output=True, text=True)
    if current.returncode != 0: return
    require(current.stdout.strip() == str(binary), 'PID no longer belongs to owned executable')
    os.kill(pid, signal.SIGTERM)
    if process is not None: process.wait(20)
    else:
        for _ in range(100):
            if pid not in processes(binary): return
            time.sleep(.1)
        raise ValueError('Owned new process did not stop; no global cleanup attempted')


def main(spec_path, workspace, target):
    require(sys.platform == 'darwin', 'Native macOS required')
    root = retention.runner_root(workspace)
    require(os.environ.get('GITHUB_REPOSITORY') == 'ousir0/osir-client', 'Authorized repository required')
    require(target in ('darwin-aarch64','darwin-x86_64'), 'Native Mac platform required')
    arch = 'arm64' if target == 'darwin-aarch64' else 'x86_64'
    require(platform.machine() == arch, 'Rosetta/cross-architecture run refused')
    native = json.loads((root / 'runtime-input.json').read_text())
    require(native['platform'] == target, 'Prepared platform mismatch')
    out = root / 'evidence'
    events = []
    def event(phase): events.append({'phase': phase, 'at': stamp()})
    process = None
    new_pid = None
    binary = None
    clicked = False
    stage = 'gui-preflight'
    try:
        require(not any(Path(row.strip()).name == 'osir_client' for row in run(['/bin/ps','-axo','comm=']).splitlines()), 'Preexisting Client refused')
        # A dedicated GitHub account must have no existing OSIR data/instance.
        require(not (Path.home() / '.osir_client').exists(), 'Preexisting system-home OSIR data refused')
        gui = root / 'install/gui-probe'
        run(['/usr/bin/xcrun','swiftc', HERE/'helpers/gui-preflight.swift','-o',gui])
        access = json.loads(run([gui]))
        save(out/'gui-preflight.json', access)
        require(access['consoleSession'] and access['accessibilityTrusted'], 'BLOCKED: native GUI/Accessibility unavailable; no permission controls changed')
        click = root / 'install/click'
        probe = root / 'install/window-probe'
        run(['/usr/bin/xcrun','swiftc',HERE/'helpers/click.swift','-o',click])
        run(['/usr/bin/xcrun','swiftc',HERE/'reviewed/osir-macos-window-probe.swift','-o',probe])
        app = root / 'install/OSIR Client.app'
        stage = 'baseline-layout'
        with tarfile.open(native['baseline']) as archive:
            total = 0
            for member in archive.getmembers():
                parts = Path(member.name).parts
                require(parts and parts[0] == 'OSIR Client.app' and '..' not in parts and not member.name.startswith('/') and '\\' not in member.name, 'Archive path escaped expected app')
                require(not member.isdev() and not member.isfifo() and not member.islnk(), 'Unsafe archive member')
                if member.issym():
                    require(not Path(member.linkname).is_absolute(), 'Absolute archive symlink refused')
                    require((root/'install'/member.name).parent.joinpath(member.linkname).resolve().is_relative_to(app), 'Archive symlink escaped app')
                total += member.size
                require(total < 2 * 1024**3, 'Archive expansion limit')
        run(['/usr/bin/tar','-xzf',native['baseline'],'-C',root/'install'])
        binary = app/'Contents/MacOS/osir_client'
        require(digest(binary) == native['old']['executableSha256'], 'Old executable bytes differ')
        require(run(['/usr/bin/lipo','-archs',binary]) == arch, 'Old architecture differs')
        run(['/usr/bin/codesign','--verify','--deep','--strict',app])
        with (app/'Contents/Info.plist').open('rb') as f: old_info = plistlib.load(f)
        require(old_info['CFBundleIdentifier'] == 'com.osir.client' and old_info['CFBundleShortVersionString'] == '0.1.10', 'Old bundle identity differs')
        require(not processes(binary), 'Preexisting owned-path process')
        settings = root/'home/.osir_client/settings.json'
        settings.parent.mkdir()
        save(settings, {**retention.SETTINGS, 'checkToolUpdatesOnStartup':False, 'unifyCodexSessionHistory':False})
        allowed = ['PATH','LANG','LC_ALL','TMPDIR','USER','LOGNAME','SHELL','HOME','__CF_USER_TEXT_ENCODING']
        env = {k:os.environ[k] for k in allowed if k in os.environ}
        env.update(CC_SWITCH_TEST_HOME=str(root/'home'), CODEX_HOME=str(root/'home/.codex'), CLAUDE_CONFIG_DIR=str(root/'home/.claude'), OPENCODE_CONFIG_DIR=str(root/'home/.config/opencode'))
        log = (out/'application.log').open('x', encoding='utf-8')
        def launch_old():
            require(not clicked, 'Harness runtime launch after update click refused')
            require(digest(binary)==native['old']['executableSha256'], 'Baseline replaced before controlled launch')
            return subprocess.Popen([str(binary)], env=env, stdout=log, stderr=log)
        stage = 'baseline-initialization'
        process = launch_old()
        schema_ready(root/'home/.osir_client/osir_client.db',process)
        time.sleep(3)
        window = json.loads(run([probe,process.pid,'com.osir.client']))
        require(window['bundlePath'] == str(app) and window['windows'] and window['otherInstancePids'] == [], 'Old native window/identity unavailable')
        save(out/'old-window.json',window);event('old-ready')
        stop_owned(process.pid,binary,process)
        retention.seed(root);event('fixture-seeded')
        stage='update-click'
        process=launch_old();old_pid=process.pid
        schema_ready(root/'home/.osir_client/osir_client.db',process)
        time.sleep(3);event('old-ready-with-fixture')
        run(['node',HERE/'contract.mjs','recheck-feed',spec_path,root,target])
        # Existing Accessibility driver performs the real shipped update button.
        with (out/'ui-click.log').open('x') as click_log:
            result=subprocess.run([str(click),str(old_pid),str(app),'0.1.11'], stdout=click_log,stderr=subprocess.STDOUT,timeout=115)
        require(result.returncode==0,'Actual update button click failed; no installer/manual restart fallback')
        click_proof=json.loads((out/'ui-click.log').read_text())
        require(click_proof['offeredVersion']=='0.1.11' and click_proof['pid']==old_pid and click_proof['bundlePath']==str(app),'Actual UI click identity differs')
        save(out/'updater-click.json',click_proof)
        clicked=True;event('update-clicked')
        stage='automatic-restart'
        deadline=time.monotonic()+20*60
        while time.monotonic()<deadline:
            if process.poll() is not None:
                if not any(e['phase']=='old-exited' for e in events): event('old-exited')
                candidates=processes(binary)
                require(len(candidates)<=1,'Ambiguous new runtime PID')
                if candidates and candidates[0]!=old_pid and digest(binary)==native['executableSha256']:
                    new_pid=candidates[0];event('new-observed');break
            time.sleep(1)
        require(new_pid is not None,'Old process did not exit and automatically launch verified new bytes')
        require(run(['/usr/bin/lipo','-archs',binary]) == arch,'Updated architecture differs')
        with (app/'Contents/Info.plist').open('rb') as f: new_info=plistlib.load(f)
        require(new_info['CFBundleShortVersionString']=='0.1.11' and new_info['CFBundleIdentifier']=='com.osir.client','Actual new bundle metadata differs')
        run(['/usr/bin/codesign','--verify','--deep','--strict',app])
        stage='updated-window'
        window=None
        for _ in range(90):
            observed=json.loads(run([probe,new_pid,'com.osir.client']))
            if observed['bundlePath']==str(app) and observed['windows'] and observed['otherInstancePids']==[]:
                window=observed;break
            time.sleep(1)
        require(window is not None,'Automatic new PID lacks exact-path native window')
        save(out/'updated-window.json',window);event('new-window')
        if window['screenCaptureAllowed']:
            run(['/usr/sbin/screencapture','-x','-l',str(window['windows'][0]['id']),out/'updated-window.png'])
        stage='data-retention'
        stop_owned(new_pid,binary);new_pid_stopped=new_pid;new_pid=None
        retention.verify(root);event('data-retained')
        save(out/'observation.json',dict(platform=target,fromVersion='0.1.10',toVersion='0.1.11',sourceCommit=native['sourceCommit'],sourceSha256=native['sourceSha256'],manifestSha256=native['manifestSha256'],oldPid=old_pid,newPid=new_pid_stopped,oldPath=str(binary),newPath=str(binary),architecture=arch,oldExecutableSha256=native['old']['executableSha256'],newExecutableSha256=digest(binary),realUiClick=True,oldPidExited=True,automaticRestart=True,nativeWindow=True,syntheticDataPreserved=True,nativeVersionVerified=True,signatureVerified=True,harnessLaunchedAfterClick=False,accountAuthorizationCovered=False,oldSchema=26,newSchema=26,events=events))
        run(['node',HERE/'contract.mjs','finalize',spec_path,root,target])
    except Exception as error:
        save(out/'platform-failure.json',{'status':'blocked' if str(error).startswith('BLOCKED:') else 'failed','stage':stage,'reason':str(error),'passed':False,'events':events})
        raise
    finally:
        if process is not None and process.poll() is None: stop_owned(process.pid,binary,process)
        if new_pid is not None: stop_owned(new_pid,binary)
        if 'log' in locals(): log.close()


if __name__=='__main__': main(*sys.argv[1:])
