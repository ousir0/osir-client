"""Read-only bounded readiness probe for the actual old app's owned DB."""
from pathlib import Path
import sqlite3
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'reviewed'))
import disposable
try:
    root = disposable.runner_root(sys.argv[1])
    path = disposable.owned(root, 'home/.osir_client/osir_client.db')
    if not path.is_file(): sys.exit(1)
    with sqlite3.connect(path.as_uri()+'?mode=ro',uri=True) as db:
        assert db.execute('pragma user_version').fetchone()==(26,)
        assert db.execute("select count(*) from sqlite_master where type='table' and name in ('providers','settings','profiles')").fetchone()==(3,)
except (AssertionError,ValueError,sqlite3.Error): sys.exit(1)
