"""Static/native-free tests of orchestration limits and dependency-free PE checks."""
import ast
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

HERE=Path(__file__).resolve().parent


class Harness(unittest.TestCase):
    def test_python_helpers_parse_without_running(self):
        for path in [HERE/'live-macos.py',*list((HERE/'helpers').glob('*.py'))]: ast.parse(path.read_text())

    def test_mac_launcher_is_old_only_and_guarded_after_click(self):
        source=(HERE/'live-macos.py').read_text()
        parsed=ast.parse(source)
        launches=[node for node in ast.walk(parsed) if isinstance(node,ast.Call) and isinstance(node.func,ast.Attribute) and node.func.attr=='Popen']
        self.assertEqual(len(launches),1)
        launch_function=next(node for node in ast.walk(parsed) if isinstance(node,ast.FunctionDef) and node.name=='launch_old')
        self.assertIn(launches[0],list(ast.walk(launch_function)))
        self.assertIn('require(not clicked',ast.get_source_segment(source,launch_function))
        self.assertNotIn('launch_old()',source.split("clicked=True;event('update-clicked')")[1])

    def test_windows_restart_observation_does_not_manually_launch(self):
        source=(HERE/'live-windows.ps1').read_text()
        after=source.split("$clicked=$true;Event 'update-clicked'")[1]
        self.assertNotIn('Start-Process -',after)
        self.assertNotIn('Launch-Old',after)
        self.assertNotIn('Stop-Process -Name',source)
        self.assertIn('$current.StartTime -ne $Process.StartTime',source)

    def test_reviewed_ui_implementations_reused_with_identity_checks(self):
        windows=(HERE/'helpers/check-ui.mjs').read_text()
        swift=(HERE/'helpers/click.swift').read_text()
        self.assertIn("window.__TAURI_INTERNALS__.invoke('plugin:app|version')",windows)
        self.assertIn('b.click()',windows)
        self.assertIn('AXUIElementPerformAction',swift)
        self.assertIn('AXIsProcessTrusted()',swift)
        self.assertIn('expectedVersion == "0.1.10"',swift)
        self.assertIn('candidate.bundleURL?',swift)
        # Native install/restart IPC is never directly invoked by either driver.
        for source in [windows,swift]:
            self.assertNotIn("invoke('install_update_and_restart'",source)
            self.assertNotIn('dangerousAcceptInvalidCerts',source)

    def test_clean_gui_runners_are_parallel_and_only_evidence_uploaded(self):
        workflow=(HERE/'workflow.review.yml').read_text()
        for runner in ['windows-2022','macos-15','macos-15-intel']: self.assertIn(runner,workflow)
        self.assertIn('fail-fast: false',workflow)
        self.assertIn('needs: gate',workflow)
        self.assertIn('cutover_confirmed',workflow)
        self.assertIn('default: false',workflow)
        self.assertIn('path: ${{ steps.workspace.outputs.path }}/evidence',workflow)
        self.assertNotIn('workflow_run:',workflow)
        self.assertNotIn('self-hosted',workflow)
        self.assertNotIn('cargo ',workflow)

    def test_dependencies_copied_with_reviewed_digests(self):
        import hashlib
        for record in json.loads((HERE/'reviewed-provenance.json').read_text()):
            path=HERE/record['path']
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(),record['sha256'])

    def test_windows_checkout_and_spec_preserve_exact_utf8_bytes(self):
        self.assertIn('* -text',(HERE/'.gitattributes').read_text())
        workflow=(HERE/'workflow.review.yml').read_text()
        self.assertEqual(workflow.count("write_bytes(raw.encode('utf-8'))"),2)
        self.assertNotIn('write_text(raw',workflow)

    def test_native_pe_machine_probe_rejects_relabelled_arm(self):
        with tempfile.TemporaryDirectory() as tmp:
            file=Path(tmp)/'fixture.exe'
            data=bytearray(128);data[:2]=b'MZ';struct.pack_into('<I',data,60,64);data[64:68]=b'PE\0\0'
            for machine,expected in [(0x8664,0),(0xaa64,1),(0x14c,1)]:
                struct.pack_into('<H',data,68,machine);file.write_bytes(data)
                p=subprocess.run([sys.executable,str(HERE/'helpers/pe-arch.py'),str(file)],capture_output=True)
                self.assertEqual(p.returncode,expected)


if __name__=='__main__':unittest.main()
