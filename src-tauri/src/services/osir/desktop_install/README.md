# Isolated transaction adapter — not a production installer

transaction.rs contains Journal, Backend, Verifier, begin, recover and cleanup_allowed. No module registration, Cargo dependency change, UI/IPC entry, system command, production filesystem backend or accepting verifier is provided. Tests use memory only and a serialized JSON fixture; /fixture-only paths are labels, never accessed.

Main may register this file with #[path = "desktop_install/transaction.rs"] pub(crate) mod desktop_install; in the existing OSIR module. Required existing dependencies: serde with derive and serde_json. transaction_tests.rs is automatically included under cfg(test). No mod.rs was added. The vendored engine manifest/path dependency is a separate main-agent step; the adapter does not depend on it yet.

The host must supply a trusted root independently from journal data, a process-wide and cross-process exclusive lease, private same-volume staging, no-follow path ownership checks, atomic durable journal replacement (no missing-primary gap), fsync of files and affected directory entries, and bounded journal loading. Unknown schema, failed ownership checks or incomplete journal scans must block cleanup. Never pass untrusted IPC paths. All pending/manual journals and surviving terminal receipts protect both descendants and ancestors from cleanup.

Production Verifier must recheck codesign strict/deep, Codex bundle ID, pinned OpenAI team, Gatekeeper and expected build. Download bytes must separately pass pinned Sparkle Ed25519 verification before entering this state machine. No production verifier is supplied in this slice.

begin journals both fresh and replacement installs before renaming. recover examines durable journal + layout and verifies candidate before continuing, verifies installed target before completing, verifies backup before rollback, and persists rollback intent before consuming backup. Ambiguous states become Manual. Cleanup never automatically deletes receipts. A future explicit receipt-retirement operation must validate the terminal state while holding the same lease.

This root-local layout is intentionally isolated; mapping a real Applications target to private staging requires a separately reviewed backend. No claim of real-app installation or power-loss recovery acceptance is made. Main should run focused tests after wiring; none were executed here per instruction.

## Filesystem implementation follow-up

fs_backend.rs now implements Backend for macOS/Linux using existing libc; no new dependency is required. transaction.rs registers fs_backend and signature_verifier as child modules. Existing host registration remains unchanged. SystemVerifier rejects unsupported platforms. Tests use existing tempfile plus injected fixture verifiers; they never invoke codesign/spctl.

API: FsBackend::open(trusted_existing_private_root), prepare(id, previous_build, target_build), load(id), scan(). Keep the backend alive across begin/recover to retain its nonblocking flock. Root must be a canonical absolute path with no symlink component, owned by the effective uid, mode 0700. No root creation, chmod of user directories, /Applications discovery, download or launch is performed. This still uses the isolated root-local layout; it is not an /Applications installer.

Operations use directory fds/openat/O_NOFOLLOW, exclusive rename, bounded journal reads/scans and atomic journal replacement without moving away the primary. Files/tree and modified directory entries are synced. Corrupt/oversized/unknown journals and interrupted .pending files fail closed; they need explicit future recovery tooling, not deletion. Terminal receipts are retained. Advisory flock coordinates cooperating processes; a malicious process running as the same uid is outside this lock's protection, especially while path-based system signature tools run. Do not expose raw Backend mutations to IPC. Retiring stale terminal receipts before subsequent installs remains a host responsibility.

SystemVerifier uses absolute system tool paths and fixed arguments, 120-second process deadlines and 64 KiB per-output bounds. It checks pinned team/bundle identity, sealed plist build, Gatekeeper, and signature again after assessment. No process-wide rlimit changes are made. Existing source trust constants were checked locally against the vendored engine. No Cargo builds/tests or actual system verification commands have been run in this task.

Earlier paragraphs describing the absence of a filesystem backend/verifier describe the initial slice and are superseded by this follow-up. Production coordination, receipt retirement and power-loss acceptance remain unimplemented/unverified.

## Saved restore points (current contract)

Completed upgrades retain their verified old.app; neither finish nor Completed recovery deletes it. Completed recovery verifies current target and previous backup. RolledBack recovery verifies previous build with no backup, or absence of both install/backup for fresh rollback; mismatch becomes Manual. Explicit restore consumes its selected backup through the durable RollingBack protocol (there is no automatic restore or backup garbage collection).

Before the next upgrade, call retire_receipt with the durable terminal Journal and verifier while holding FsBackend. Retirement revalidates the terminal layout and changes the step to RetiredCompleted/RetiredRolledBack. It DOES NOT delete receipt, staging or saved backup. prepare/create reject every non-retired receipt. Retired recovery checks only its own backup, never the shared current installation. Pending/current ownership checks reject stale receipt copies and prevent an old receipt from acting during a newer pending transaction. Old backup materials remain cleanup-protected even after retirement. No generic deletion/retention policy is added.

Restore API: preflight_restore returns RestoreExpectation {receipt_id,target_build,previous_build}; restore_point rechecks the exact expectation, current installed target, saved previous build and competing receipts before recording rollback intent. A retired restore point can be selected only when no other non-retired receipt owns the installation and the current build still equals its original target. A newer/different current target is rejected. Receipt retirement and a later restore require explicit caller intent; do not retire all receipts automatically during startup.

Backend now requires check_current(&Journal). FsBackend checks durable equality and exclusive receipt authority; custom/mock backends must implement it too. New filesystem fixture tests cover two consecutive upgrades with explicit retirement, reopening during the second rename window, old/current receipt isolation, changed-target rejection, terminal verification and backup preservation. No Cargo/test execution was performed in this change.

## Offline release boundary

release.rs is registered only on macOS, using the main agent's engine path dependency and existing url/base64/serde. appcast_url(Source, Architecture) returns the four exact URLs from Manager mac_update.rs:110-118. There is no custom URL setting or network request. parse_release checks the supplied response URL, bounded XML, highest snapshot build (duplicate highest rejected), full enclosure URL, nonzero bounded size and 64-byte base64 signature. Highest snapshot build is not a freshness guarantee. Architecture is bound to the requested feed, not proven by artifact metadata; real bundle architecture and minimum-OS compatibility still require the install coordinator's checks.

ReleaseMetadata is serializable but explicitly unverified release metadata. ReleasePlan and VerifiedArtifact are native-only, cannot be constructed/deserialized from IPC. verify_artifact requires exact bounded bytes and the engine's pinned Sparkle Ed25519 signature. It does not prove safe extraction or a valid Codex bundle; existing SystemVerifier must still validate the reconstructed bundle. No BinaryDelta tool is present, so every plan selects the full ZIP regardless of delta entries.

A future downloader MUST disable automatic redirects, validate each hop before requesting it, enforce streaming size/time limits and carry the same ReleasePlan through verification. Feed redirects are rejected; artifact redirects are limited to the original allowlisted origin, HTTPS, ZIP paths and fewer than five redirect hops. Credentials, explicit ports (including 443), queries, fragments, percent-encoded paths and normalization ambiguity are rejected. No additional CDN hosts are guessed. Engine's generic curl downloader does not enforce this policy and must not be wired directly as an unrestricted redirecting fetch.

fixtures/release-appcast.xml is copied unchanged from the vendored engine's tests/fixtures/appcast.xml (source provenance remains in vendor SOURCE_PROVENANCE.json). It is a historical offline snapshot, not current release data. Some upstream delta signatures are placeholders; none are selected. Six offline tests cover full selection, source/arch binding, URL rejection, bounds, missing signatures and forged-artifact rejection. No matching full official package is present, so a positive pinned-key artifact verification test is not claimed; the engine's existing deterministic signing tests separately exercise successful cryptographic verification. No Cargo, network, installation or system-signature commands were run.

### Native download boundary

macOS child module: transaction::download. Keep one ReleaseTransport per native
application lifetime; clones share one download permit and the feed cache.
- ReleaseTransport::new(): internal HTTPS-only reqwest client, no proxy, automatic
  redirects, retries or decompression. No arbitrary client/URL injection API.
- fetch_release(Source, Architecture, current_build, &Cancellation): on-demand,
  30-second total deadline, no feed redirects, 8 MiB maximum XML; four-entry cache
  (60-second TTL), 15-second global / 60-second per-pair request interval. Failed
  or cancelled requests count. Cached XML is reparsed against current_build.
- download(ReleasePlan, &private_cache_directory, &Cancellation): one active
  operation; 600-second total deadline, 30-second idle read timeout. Every artifact
  redirect is explicitly checked before sending. Exact declared size (maximum
  2 GiB), identity encoding, private temporary file, sync, unlink, read-only mmap
  and the engine's pinned Sparkle verification precede publication.
- VerifiedFile::file()/build()/size(): native-only read handle and metadata;
  dropping it cleans staging. It has no IPC serialization or installation method.
- Cancellation::cancel(): sticky; dropping the future also drops network/staging.
  An already-running crypto verification retains its permit and staging until it
  exits; cancellation never publishes its result. Chunk file writes are synchronous
  bounded writes inside the async operation; filesystem stalls are not preemptible.

The installer service must consume the read handle to extract into its isolated
transaction staging, then run bundle/team/build/SystemVerifier checks before the
existing journaled commit. Download authentication alone does not authorize an
installation. Feed metadata is not an authenticated freshness/architecture claim.
No production app or configuration path is selected here. Private cache directory
ownership and mode are checked; its parent hierarchy must remain native-owned.

Offline download_tests cover loopback streaming, rejected size/encoding/redirect,
unlinked files, pinned signature rejection, sticky cancellation and cached build
re-evaluation/rate admission. They were authored, not executed by this agent.
No positive package fixture signed by the production key is bundled. No Cargo,
real download, system signature command or installation was performed.
