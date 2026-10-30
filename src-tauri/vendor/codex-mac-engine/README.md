# Vendored library snapshot

Source and SHA-256 file inventory: SOURCE_PROVENANCE.json. MIT license retained in LICENSE. Library files and upstream parser tests/fixtures are unchanged. src/bin, executable assets, Cargo.toml and Cargo.lock were intentionally omitted per task scope.

WARNING: upstream lib.rs contains stale planning-only documentation. This library contains download, quit, replacement and launch APIs. None are wired into the host by this change. BinaryDelta executable is not included.

Main integration must create a library-only Cargo.toml with autobins=false, edition=2021, package codex-mac-engine 0.1.0 and license=MIT. Source dependency declarations: roxmltree 0.21; thiserror 2.0; serde 1.0 (derive); ed25519-dalek 3; base64 0.22; log 0.4; uuid 1 (v4); libc 0.2. Resolve compatibility in the main integration, not by upgrading host dependencies wholesale.

No build, test executable, network fetch or installer was run during vendoring.

## Host integration verification (2026-10-27)

The host subsequently added a library-only Cargo.toml (`autobins=false`) and a macOS-only path dependency. The 14 source/provenance hashes were verified unchanged. Offline single-job engine tests passed: 19 unit tests and 5 appcast/planning tests. The host offline Cargo check also passed. Only cached dependencies were used; no app download, installation, launch or real system signature acceptance was performed. Upstream swap APIs are not the OSIR installation controller: OSIR uses the separate durable transaction and saved-restore-point boundary under services/osir/desktop_install.
