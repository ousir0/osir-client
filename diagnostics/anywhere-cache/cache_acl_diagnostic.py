import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import tempfile

out = Path(os.environ['DIAGNOSTIC_OUTPUT'])
out.mkdir(parents=True, exist_ok=True)
src = Path('src-tauri/src/commands/osir_anywhere_install_io.rs').read_text(encoding='utf-8')
script = re.search(r'let script = format!\(\s*r#"(.*?)"#,\s*ps_literal', src, re.S).group(1)
script = script.replace('{{', '{').replace('}}', '}')
buf = ctypes.create_unicode_buffer(32768)
assert ctypes.windll.kernel32.GetSystemDirectoryW(buf, len(buf))
exe = Path(buf.value) / 'WindowsPowerShell/v1.0/powershell.exe'
prelude = "$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false);\n"

def run(code, env):
    p = subprocess.run([str(exe), '-NoLogo', '-NoProfile', '-NonInteractive', '-Command', prelude + code], env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=30)
    return dict(exitCode=p.returncode, stdout=p.stdout.decode('utf-8', 'replace'), stderr=p.stderr.decode('utf-8', 'replace'))

report = dict(platform=platform.platform(), executable=str(exe), sourceSha256=hashlib.sha256(src.encode()).hexdigest(), inheritedPSModulePath=os.environ.get('PSModulePath'), cases=[])
info = r'''
@{version=$PSVersionTable.PSVersion.ToString(); edition=$PSVersionTable.PSEdition; psHome=$PSHOME; modulePath=$env:PSModulePath; identity=[Security.Principal.WindowsIdentity]::GetCurrent().Name; sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value; elevated=([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator); os=[Environment]::OSVersion.VersionString} | ConvertTo-Json -Compress
'''
report['host'] = run(info, dict(os.environ))
for mode in ('inherited', 'module_path_removed', 'system_modules_only'):
    env = dict(os.environ)
    if mode == 'module_path_removed': env.pop('PSModulePath', None)
    if mode == 'system_modules_only': env['PSModulePath'] = str(exe.parent / 'Modules')
    with tempfile.TemporaryDirectory(prefix='osir-cache-diag-') as tmp:
        literal = "'" + tmp.replace("'", "''") + "'"
        code = script.replace('$path = {}', '$path = ' + literal)
        result = run(code, env)
        probe = r'''
$path = REPLACE
$d=[IO.Directory]::GetAccessControl($path)
@{owner=$d.GetOwner([Security.Principal.SecurityIdentifier]).Value; protected=$d.AreAccessRulesProtected; sddl=$d.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::All); securityModules=@(Get-Module -ListAvailable Microsoft.PowerShell.Security | ForEach-Object { $_.Path })} | ConvertTo-Json -Depth 4 -Compress
'''.replace('REPLACE', literal)
        result['aclAfter'] = run(probe, env)
        report['cases'].append(dict(mode=mode, **result))
(out / 'report.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
print(json.dumps(report, indent=2))
