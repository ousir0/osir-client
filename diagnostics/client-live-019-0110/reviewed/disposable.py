"""Fixture and HTTPS-feed helpers; no app/process/installer control is implemented."""
import argparse
from contextlib import closing
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sqlite3
import ssl
import sys
from datetime import datetime, timezone

MARKER = "osir-updater-019-to-0110-owned.json"
FIXTURE = "019-to-0110-synthetic-only"
SETTINGS = {
    "language": "zh", "showProfileSwitcher": True, "sessionAutoSyncEnabled": False,
    "launchOnStartup": False, "enableLocalProxy": False, "enableStackMode": False,
    "minimizeToTrayOnClose": False,
}
USER_FILES = {
    ".codex/config.toml": b'model = "osir-updater-fixture"\n# 019-to-0110: no login, no model request\n',
    ".claude/settings.json": b'{"permissions":{"allow":[],"deny":[]}}\n',
}


def require(value, message):
    if not value:
        raise ValueError(message)


def digest(path):
    checksum = hashlib.sha256()
    with Path(path).open("rb") as stream:
        while block := stream.read(1024 * 1024):
            checksum.update(block)
    return checksum.hexdigest()


def save(path, value):
    with Path(path).open("x", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")


def runner_root(path, *, existing=True):
    require(os.environ.get("GITHUB_ACTIONS") == "true" and os.environ.get("CI") == "true",
            "Only disposable GitHub Actions runners are allowed; local execution refused")
    require(os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted", "Self-hosted machines refused")
    require(os.environ.get("RUNNER_TEMP") and os.environ.get("GITHUB_RUN_ID"), "Runner identity missing")
    temporary = Path(os.environ["RUNNER_TEMP"]).resolve(strict=True)
    raw = Path(path)
    require(raw.is_absolute(), "Absolute runner-owned workspace required")
    root = raw.resolve()
    require(root != temporary and root.is_relative_to(temporary), "Workspace must be strictly below RUNNER_TEMP")
    require(root.name.startswith("osir-updater-019-to-0110-"), "Dedicated updater workspace name required")
    require(not raw.is_symlink(), "Symlink workspace refused")
    if existing:
        marker = json.loads((root / MARKER).read_text())
        require(marker == {"runId": os.environ["GITHUB_RUN_ID"], "purpose": FIXTURE}, "Workspace ownership mismatch")
    return root


def owned(root, relative):
    target = root / relative
    require(target.resolve().is_relative_to(root), "Fixture path escaped workspace")
    current = target
    while current != root:
        require(not current.is_symlink(), "Symlink fixture refused")
        current = current.parent
    return target


def initialize(path):
    root = runner_root(path, existing=False)
    root.mkdir(mode=0o700)
    for directory in ("home", "evidence", "private", "install"):
        (root / directory).mkdir(mode=0o700)
    save(root / MARKER, {"runId": os.environ["GITHUB_RUN_ID"], "purpose": FIXTURE})
    return {"status": "workspace-prepared-not-executed", "root": str(root)}


def fixture_state(root):
    db = owned(root, "home/.osir_client/osir_client.db")
    require(db.is_file(), "Initialize schema with the actual 0.1.9 app first; helper never creates an app database")
    with closing(sqlite3.connect(db.as_uri() + "?mode=ro", uri=True)) as conn:
        require(conn.execute("pragma quick_check").fetchall() == [("ok",)], "Database integrity failed")
        state = {
            "provider": conn.execute("select id,app_type,name,settings_config,meta,sort_index,is_current from providers where id=?", (FIXTURE,)).fetchall(),
            "profile": conn.execute("select id,name,payload,sort_order,created_at,updated_at from profiles where id=?", (FIXTURE,)).fetchall(),
            "sentinel": conn.execute("select value from osir_updater_acceptance").fetchall(),
        }
        version = conn.execute("pragma user_version").fetchone()[0]
    device = json.loads(owned(root, "home/.osir_client/settings.json").read_text())
    state["settings"] = {key: device.get(key) for key in SETTINGS}
    state["userConfig"] = {name: digest(owned(root, "home/" + name)) for name in USER_FILES}
    return {"state": state, "schemaVersion": version, "databaseSha256": digest(db)}


def seed(path):
    root = runner_root(path)
    before = owned(root, "evidence/before.json")
    require(not before.exists(), "Fixture already seeded")
    db = owned(root, "home/.osir_client/osir_client.db")
    require(db.is_file(), "Actual baseline initialization required")
    # The caller must stop only its proven baseline PID before seeding.
    with closing(sqlite3.connect(db)) as conn:
        require(conn.execute("pragma user_version").fetchone() == (25,), "Expected actual 0.1.9 schema v25")
        require(conn.execute("pragma quick_check").fetchall() == [("ok",)], "Database integrity failed")
        conn.execute("create table osir_updater_acceptance(value text not null)")
        conn.execute("insert into osir_updater_acceptance values(?)", (FIXTURE,))
        conn.execute("insert into providers(id,app_type,name,settings_config,meta,sort_index,is_current) values(?,?,?,?,?,?,?)",
                     (FIXTURE, "codex", "合成更新验收供应商", '{"model":"fixture-model"}', '{}', 42, 0))
        payload = json.dumps({"providers": {"codex": FIXTURE}, "mcp": {"codex": []}, "skills": {"codex": []}, "prompts": {"codex": None}}, ensure_ascii=False)
        conn.execute("insert into profiles(id,name,payload,sort_order,created_at,updated_at) values(?,?,?,?,?,?)",
                     (FIXTURE, "合成项目 019→0110", payload, 42, 1794614400, 1794614400))
        conn.commit()
        conn.execute("pragma wal_checkpoint(truncate)")
    settings = owned(root, "home/.osir_client/settings.json")
    device = json.loads(settings.read_text()) if settings.exists() else {}
    device.update(SETTINGS)
    settings.write_text(json.dumps(device, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    for name, content in USER_FILES.items():
        target = owned(root, "home/" + name)
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("xb") as stream:
            stream.write(content)
    observed = fixture_state(root)
    require(len(observed["state"]["provider"]) == len(observed["state"]["profile"]) == 1, "Fixture rows missing")
    save(before, observed)
    return {"status": "synthetic-fixture-seeded-not-updater-acceptance", "before": str(before)}


def verify(path):
    root = runner_root(path)
    before_path = owned(root, "evidence/before.json")
    before = json.loads(before_path.read_text())
    after = fixture_state(root)
    before_state = json.loads(json.dumps(before["state"]))
    after_state = json.loads(json.dumps(after["state"]))
    changed = {}
    for key in sorted(set(before_state) | set(after_state)):
        if before_state.get(key) != after_state.get(key):
            old = json.dumps(before_state.get(key), ensure_ascii=False, sort_keys=True).encode()
            new = json.dumps(after_state.get(key), ensure_ascii=False, sort_keys=True).encode()
            changed[key] = {"beforeSha256": hashlib.sha256(old).hexdigest(),
                            "afterSha256": hashlib.sha256(new).hexdigest()}
    if after["schemaVersion"] != 26 or changed:
        diagnostic = {"status": "retention-diagnostic", "beforeSchemaVersion": before["schemaVersion"],
                      "afterSchemaVersion": after["schemaVersion"], "changedStateSections": changed}
        save(owned(root, "evidence/data-retention-diagnostic.json"), diagnostic)
    # SQLite layout/hash may legitimately change during migration; compare actual logical records.
    require(after["schemaVersion"] == 26, "Expected 0.1.10 schema v26; review this contract if candidate changes")
    require(before_state == after_state, "Synthetic profile/provider/settings/user config changed")
    result = {"status": "data-retention-check-only", "scope": "Does not prove updater, version, restart, window or release acceptance",
              "beforeSha256": digest(before_path), "before": before, "after": after}
    save(owned(root, "evidence/data-retention.json"), result)
    return {"status": result["status"]}


def feed_allowed(host, path):
    return host in ("osirclaw.com", "osirclaw.com:443") and path == "/api/v1/osir-client/updater/prod.json"


def serve(path, feed_path):
    root = runner_root(path)
    preparation = Path(feed_path).parent / "preparation.json"
    proof = json.loads(preparation.read_text())
    require(proof["status"] == "prepared-not-executed" and proof.get("sourceSha256"), "Candidate signature/public-byte preparation is required")
    payload = Path(feed_path).read_bytes()
    require(hashlib.sha256(payload).hexdigest() == proof["feed"]["sha256"], "Feed changed after artifact preparation")
    feed = json.loads(payload)
    require(feed["version"] == "0.1.10", "Exact cross-version candidate required")
    require(set(feed["platforms"]) == {"darwin-aarch64", "darwin-x86_64", "windows-x86_64"}, "Feed platform set differs")
    by_platform = {item["platform"]: item for item in proof["artifacts"]}
    for platform, value in feed["platforms"].items():
        artifact = by_platform[platform]
        require(artifact["publicReadback"]["url"] == value["url"] and artifact["publicReadback"]["readback"] == "complete", "Feed differs from public readback")
        require(artifact["signatureVerified"] is True, "Candidate signature was not verified")
    cert = owned(root, "private/feed-chain.pem")
    key = owned(root, "private/feed.key")
    log_path = owned(root, "evidence/loopback-requests.jsonl")
    require(not log_path.exists(), "Do not reuse prior request evidence")

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            allowed = feed_allowed(self.headers.get("Host"), self.path)
            body = payload if allowed else b"Not found\n"
            self.send_response(200 if allowed else 404)
            self.send_header("Content-Type", "application/json" if allowed else "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)
            if allowed:
                with log_path.open("a", encoding="utf-8") as stream:
                    stream.write(json.dumps({"time": datetime.now(timezone.utc).isoformat(), "route": "candidate-feed", "feedSha256": hashlib.sha256(payload).hexdigest()}) + "\n")

        def log_message(self, *_):
            pass  # Never record arbitrary paths, query strings, headers or credentials.

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(cert, key)
    server = ThreadingHTTPServer(("127.0.0.1", 443), Handler)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    print("Loopback candidate metadata ready; artifact bytes still go directly to RainS3", flush=True)
    server.serve_forever()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("init", "seed", "verify", "serve"))
    parser.add_argument("workspace")
    parser.add_argument("feed", nargs="?")
    args = parser.parse_args()
    if args.command == "serve":
        require(args.feed, "Prepared candidate feed required")
        serve(args.workspace, args.feed)
    else:
        result = {"init": initialize, "seed": seed, "verify": verify}[args.command](args.workspace)
        print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
