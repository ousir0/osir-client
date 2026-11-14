"""Helper-only unit tests. No application or real migration is executed."""
from contextlib import closing
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch
import disposable as helper


class Helpers(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.parent = Path(self.temp.name).resolve()
        self.root = self.parent / "osir-updater-0110-to-0111-unit-only"
        self.env = patch.dict(os.environ, {"GITHUB_ACTIONS":"true", "CI":"true", "RUNNER_ENVIRONMENT":"github-hosted", "RUNNER_TEMP":str(self.parent), "GITHUB_RUN_ID":"unit-test"})
        self.env.start()
        self.addCleanup(self.env.stop)

    def schema_fixture(self):
        helper.initialize(self.root)
        db = self.root / "home/.osir_client/osir_client.db"
        db.parent.mkdir()
        with closing(sqlite3.connect(db)) as conn:
            conn.executescript("create table providers(id text,app_type text,name text,settings_config text,meta text,sort_index integer,is_current integer); create table profiles(id text,name text,payload text,sort_order integer,created_at integer,updated_at integer); pragma user_version=26;")
        return db

    def test_local_self_hosted_and_external_paths_are_refused(self):
        for key,value in [("GITHUB_ACTIONS","false"),("CI","false"),("RUNNER_ENVIRONMENT","self-hosted")]:
            with patch.dict(os.environ,{key:value}):
                with self.assertRaises(ValueError): helper.initialize(self.root)
        with self.assertRaises(ValueError): helper.initialize(self.parent.parent / self.root.name)
        self.assertFalse(self.root.exists())

    def test_fresh_workspace_and_run_identity_required(self):
        helper.initialize(self.root)
        with self.assertRaises(FileExistsError): helper.initialize(self.root)
        with patch.dict(os.environ,{"GITHUB_RUN_ID":"another-run"}):
            with self.assertRaises(ValueError): helper.runner_root(self.root)

    def test_symlink_home_is_refused(self):
        helper.initialize(self.root)
        (self.root / "home").rmdir()
        (self.root / "home").symlink_to(self.parent, target_is_directory=True)
        with self.assertRaises(ValueError): helper.owned(self.root,"home/.osir_client/osir_client.db")

    def test_fixture_keeps_profile_and_userconfig_and_detects_tampering(self):
        db=self.schema_fixture()
        helper.seed(self.root)
        with closing(sqlite3.connect(db)) as conn: conn.execute("pragma user_version=26")  # Unit fixture only, not a product migration.
        self.assertEqual(helper.verify(self.root)["status"],"data-retention-check-only")
        (self.root / "home/.codex/config.toml").write_text("changed")
        with self.assertRaisesRegex(ValueError,"changed"): helper.verify(self.root)

    def test_missing_actual_initialized_db_is_refused(self):
        helper.initialize(self.root)
        with self.assertRaises(ValueError): helper.seed(self.root)
        self.assertFalse((self.root / "home/.osir_client/osir_client.db").exists())

    def test_0110_schema26_retains_without_migration(self):
        self.schema_fixture()
        helper.seed(self.root)
        self.assertEqual(helper.verify(self.root)["status"], "data-retention-check-only")
        import json
        report=json.loads((self.root/'evidence/data-retention.json').read_text())
        self.assertEqual(report['before']['schemaVersion'],26)
        self.assertEqual(report['after']['schemaVersion'],26)
        self.assertEqual(report['before']['state'],report['after']['state'])

    def test_0110_schema25_cannot_be_accepted_as_new_baseline(self):
        db=self.schema_fixture()
        with closing(sqlite3.connect(db)) as conn: conn.execute('pragma user_version=25')
        with self.assertRaises(ValueError): helper.seed(self.root)

    def test_feed_only_serves_exact_host_and_path(self):
        self.assertTrue(helper.feed_allowed("osirclaw.com:443","/api/v1/osir-client/updater/prod.json"))
        for host,path in [("evil.example","/api/v1/osir-client/updater/prod.json"),("osirclaw.com","/api/v1/osir-client/updater/prod.json?token=x"),("osirclaw.com","/client-release/x.exe")]:
            self.assertFalse(helper.feed_allowed(host,path))


if __name__ == "__main__":
    unittest.main()
