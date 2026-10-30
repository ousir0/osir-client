//! Explicit real-package acceptance. No mock transport/verifier, launches, HOME
//! overrides, trust changes, or writes to any pre-existing installation.
use super::*;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

type AcceptanceResult<T> = std::result::Result<T, &'static str>;
const BASELINE_BUILD: u64 = 21828;
const OPT_IN: &str = "OSIR_REAL_CODEX_INSTALLER_ACCEPTANCE";

#[derive(Debug, PartialEq, Eq, Serialize)]
struct FileFingerprint {
    device: u64,
    inode: u64,
    size: u64,
    sha256: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct BundleFingerprint {
    device: u64,
    inode: u64,
    build: u64,
    files: BTreeMap<String, FileFingerprint>,
}

fn require(condition: bool, error: &'static str) -> AcceptanceResult<()> {
    if condition {
        Ok(())
    } else {
        Err(error)
    }
}

fn fingerprint_file(path: &Path) -> AcceptanceResult<FileFingerprint> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "fingerprint_metadata_failed")?;
    require(metadata.is_file(), "fingerprint_not_regular_file")?;
    let mut file = File::open(path).map_err(|_| "fingerprint_open_failed")?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| "fingerprint_read_failed")?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(FileFingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.len(),
        sha256: format!("{:x}", hash.finalize()),
    })
}

fn fingerprint_bundle(app: &Path) -> AcceptanceResult<BundleFingerprint> {
    let metadata = fs::symlink_metadata(app).map_err(|_| "bundle_metadata_failed")?;
    require(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "bundle_not_directory",
    )?;
    let (_, build) = codex_mac_engine::sys::installed_codex_build_at_path(&app.to_string_lossy())
        .ok_or("bundle_build_unavailable")?;
    let mut files = BTreeMap::new();
    for relative in [
        "Contents/Info.plist",
        "Contents/_CodeSignature/CodeResources",
    ] {
        files.insert(relative.to_string(), fingerprint_file(&app.join(relative))?);
    }
    let executables =
        fs::read_dir(app.join("Contents/MacOS")).map_err(|_| "bundle_executables_unavailable")?;
    for entry in executables {
        let entry = entry.map_err(|_| "bundle_executable_entry_failed")?;
        let relative = format!("Contents/MacOS/{}", entry.file_name().to_string_lossy());
        files.insert(relative, fingerprint_file(&entry.path())?);
    }
    require(files.len() > 2, "bundle_executable_missing")?;
    Ok(BundleFingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        build,
        files,
    })
}

fn fingerprint_protected(
    paths: &BTreeSet<PathBuf>,
) -> AcceptanceResult<BTreeMap<PathBuf, BundleFingerprint>> {
    paths
        .iter()
        .map(|path| Ok((path.clone(), fingerprint_bundle(path)?)))
        .collect()
}

fn record(
    path: &Path,
    evidence: &mut Value,
    phase: &'static str,
    metadata: Value,
) -> AcceptanceResult<()> {
    evidence["phase"] = json!(phase);
    evidence[phase] = metadata;
    let pending = path.with_extension("pending");
    let bytes = serde_json::to_vec_pretty(evidence).map_err(|_| "evidence_encode_failed")?;
    fs::write(&pending, bytes).map_err(|_| "evidence_write_failed")?;
    fs::rename(&pending, path).map_err(|_| "evidence_publish_failed")?;
    eprintln!("OSIR real updater acceptance: {phase}");
    Ok(())
}

fn create_private(path: &Path) -> AcceptanceResult<()> {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|_| "create_private_failed")?;
    owned_dir(path, true).map_err(|_| "private_directory_rejected")
}

// Exhaustive static classifications: never format external response bodies,
// verifier output, URLs, or I/O errors into the acceptance evidence.
fn install_error_metadata(error: &InstallError) -> Value {
    let (kind, download_kind) = match error {
        InstallError::Busy => ("Busy", None),
        InstallError::Cancelled => ("Cancelled", None),
        InstallError::Running => ("Running", None),
        InstallError::Changed => ("Changed", None),
        InstallError::Storage => ("Storage", None),
        InstallError::InvalidPackage => ("InvalidPackage", None),
        InstallError::Verification => ("Verification", None),
        InstallError::ManualRequired => ("ManualRequired", None),
        InstallError::PermissionDenied => ("PermissionDenied", None),
        InstallError::Transport(error) => (
            "Transport",
            Some(match error {
                DownloadError::Transport => "Transport",
                DownloadError::Http => "Http",
                DownloadError::Policy => "Policy",
                DownloadError::Size => "Size",
                DownloadError::Encoding => "Encoding",
                DownloadError::Signature => "Signature",
                DownloadError::Io => "Io",
                DownloadError::Cancelled => "Cancelled",
                DownloadError::Timeout => "Timeout",
                DownloadError::Busy => "Busy",
                DownloadError::RateLimited => "RateLimited",
                DownloadError::Feed => "Feed",
            }),
        ),
    };
    json!({"kind":kind,"downloadKind":download_kind})
}

async fn record_failed_install(
    coordinator: &InstallCoordinator,
    baseline_copy: &BundleFingerprint,
    error: &InstallError,
    report_path: &Path,
    evidence: &mut Value,
) -> AcceptanceResult<()> {
    let error = install_error_metadata(error);
    // Emit the finite error immediately as well as writing it into the report,
    // so a subsequent evidence-storage failure cannot hide the original class.
    eprintln!("OSIR real updater install failure classification: {error}");
    let (inspection, recovery_required) = match coordinator.local(LocalAction::Inspect).await {
        Ok(local) => {
            let recovery_required = local.recovery_required;
            (json!({"ok":true,"report":local}), Some(recovery_required))
        }
        Err(error) => (
            json!({"ok":false,"error":install_error_metadata(&error)}),
            None,
        ),
    };
    let target = coordinator.0.root.join("Codex.app");
    let fingerprint = fingerprint_bundle(&target);
    let target_matches_baseline = fingerprint
        .as_ref()
        .ok()
        .map(|current| current == baseline_copy);
    let target_snapshot = match fingerprint {
        Ok(fingerprint) => json!({"ok":true,"fingerprint":fingerprint}),
        Err(error) => json!({"ok":false,"error":error}),
    };
    record(
        report_path,
        evidence,
        "official_install_failed",
        json!({
            "error":error,
            "localInspection":inspection,
            "recoveryRequired":recovery_required,
            "targetMatchesBaseline":target_matches_baseline,
            "targetSnapshot":target_snapshot,
        }),
    )
}

async fn exercise(
    coordinator: &InstallCoordinator,
    baseline: &Path,
    report_path: &Path,
    evidence: &mut Value,
) -> AcceptanceResult<()> {
    SystemVerifier
        .verify(baseline, BASELINE_BUILD)
        .map_err(|_| "baseline_signature_rejected")?;
    let target = coordinator.0.root.join("Codex.app");
    require(!target.exists(), "isolated_target_already_exists")?;
    let status = Command::new("/usr/bin/ditto")
        .args(["--rsrc", "--extattr", "--acl"])
        .arg(baseline)
        .arg(&target)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "baseline_copy_failed")?;
    require(status.success(), "baseline_copy_failed")?;
    SystemVerifier
        .verify(&target, BASELINE_BUILD)
        .map_err(|_| "copied_baseline_signature_rejected")?;
    let copied = fingerprint_bundle(&target)?;
    let source = fingerprint_bundle(baseline)?;
    require(
        copied.build == BASELINE_BUILD,
        "copied_baseline_wrong_build",
    )?;
    require(
        (copied.device, copied.inode) != (source.device, source.inode),
        "baseline_not_independent_copy",
    )?;
    require(
        copied.files.len() == source.files.len()
            && copied.files.iter().all(|(name, file)| {
                source.files.get(name).is_some_and(|old| {
                    old.sha256 == file.sha256
                        && (old.device, old.inode) != (file.device, file.inode)
                })
            }),
        "baseline_copy_bytes_changed",
    )?;
    record(
        report_path,
        evidence,
        "baseline_copied",
        json!({"target": target, "fingerprint": copied}),
    )?;

    let checked = coordinator
        .check(Source::Official)
        .await
        .map_err(|_| "official_check_failed")?;
    record(
        report_path,
        evidence,
        "official_checked",
        serde_json::to_value(&checked).map_err(|_| "check_metadata_failed")?,
    )?;
    require(
        checked.installed_build == Some(BASELINE_BUILD),
        "check_baseline_mismatch",
    )?;
    require(
        checked.comparison_complete,
        "installation_comparison_incomplete",
    )?;
    require(
        checked.release.update_available,
        "official_upgrade_unavailable",
    )?;
    require(
        checked.release.build > checked.highest_detected_build.unwrap_or(0),
        "candidate_not_above_highest_installation",
    )?;
    require(
        checked.release.full_size > 0,
        "official_package_size_missing",
    )?;

    let candidate = checked.release.build;
    record(
        report_path,
        evidence,
        "official_install_started",
        json!({"build":candidate,"bytes":checked.release.full_size}),
    )?;
    let installed = match coordinator.install(Source::Official, candidate).await {
        Ok(installed) => installed,
        Err(error) => {
            record_failed_install(coordinator, &copied, &error, report_path, evidence).await?;
            return Err("official_install_failed");
        }
    };
    record(
        report_path,
        evidence,
        "official_installed",
        serde_json::to_value(&installed).map_err(|_| "install_metadata_failed")?,
    )?;
    require(
        installed.step == Step::Completed && installed.build == Some(candidate),
        "install_not_completed",
    )?;

    // Always attempt the explicitly authorized restore after a completed install,
    // even if the intermediate receipt/backup acceptance assertions fail.
    let inspected = async {
        let local = coordinator
            .local(LocalAction::Inspect)
            .await
            .map_err(|_| "installed_inspection_failed")?;
        record(
            report_path,
            evidence,
            "installed_inspected",
            serde_json::to_value(&local).map_err(|_| "inspection_metadata_failed")?,
        )?;
        require(
            local.installed_build == Some(candidate) && !local.recovery_required,
            "installed_state_mismatch",
        )?;
        require(
            local.restore_points.len() == 1,
            "expected_single_restore_point",
        )?;
        let point = &local.restore_points[0];
        require(
            point.receipt_id == installed.receipt_id
                && point.target_build == candidate
                && point.previous_build == BASELINE_BUILD,
            "restore_point_mismatch",
        )?;
        let backend = FsBackend::open(&coordinator.0.root).map_err(|_| "receipt_open_failed")?;
        let journal = backend
            .load(&installed.receipt_id)
            .map_err(|_| "receipt_load_failed")?;
        let backup = fingerprint_bundle(&journal.backup)?;
        require(backup == copied, "backup_not_original_baseline")?;
        record(
            report_path,
            evidence,
            "backup_verified",
            json!({"receipt":journal.id,"fingerprint":backup}),
        )
    }
    .await;
    let restored = coordinator
        .restore(RestorePoint {
            receipt_id: installed.receipt_id,
            target_build: candidate,
            previous_build: BASELINE_BUILD,
        })
        .await
        .map_err(|_| "explicit_restore_failed")?;
    record(
        report_path,
        evidence,
        "explicitly_restored",
        serde_json::to_value(&restored).map_err(|_| "restore_metadata_failed")?,
    )?;
    require(
        restored.step == Step::RolledBack && restored.build == Some(BASELINE_BUILD),
        "restore_not_completed",
    )?;
    let local = coordinator
        .local(LocalAction::Inspect)
        .await
        .map_err(|_| "restored_inspection_failed")?;
    let restored_fingerprint = fingerprint_bundle(&target)?;
    require(
        local.installed_build == Some(BASELINE_BUILD)
            && !local.recovery_required
            && local.restore_points.is_empty(),
        "restored_state_mismatch",
    )?;
    require(
        restored_fingerprint == copied,
        "restored_baseline_identity_or_bytes_changed",
    )?;
    record(
        report_path,
        evidence,
        "restored_inspected",
        json!({"local":local,"fingerprint":restored_fingerprint}),
    )?;
    inspected
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
#[ignore = "Real official Codex download/install/restore; requires explicit OSIR_REAL_CODEX_INSTALLER_ACCEPTANCE=1"]
async fn official_codex_updater_installs_and_explicitly_restores_in_new_private_root() {
    assert_eq!(
        std::env::var(OPT_IN).as_deref(),
        Ok("1"),
        "Explicit environment opt-in is required"
    );
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    let dev_home = workspace.join(".osir-dev-home");
    owned_dir(&dev_home, false)
        .expect("Existing isolated development home must be canonical and owned");
    let baseline = dev_home.join("Applications/OSIR Codex/Codex.app");
    assert_eq!(baseline.canonicalize().unwrap(), baseline);
    let detection = crate::services::osir::codex_desktop::detect();
    assert!(
        detection.comparison_complete,
        "All standard installations must be readable"
    );
    let mut protected: BTreeSet<PathBuf> = detection
        .installations
        .iter()
        .map(|installation| PathBuf::from(&installation.path).canonicalize().unwrap())
        .collect();
    assert!(
        protected.contains(Path::new("/Applications/ChatGPT.app")),
        "The current system host must be protected"
    );
    protected.insert(baseline.clone());
    let before = fingerprint_protected(&protected).expect("Protected installation fingerprints");

    let container = dev_home.join(".installer-acceptance");
    match fs::DirBuilder::new().mode(0o700).create(&container) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => panic!("Cannot create isolated acceptance container"),
    }
    owned_dir(&container, true).expect("Acceptance container must be canonical and private");
    let project = container.join(uuid::Uuid::new_v4().to_string());
    create_private(&project).expect("Fresh acceptance project");
    let root = project.join("Applications/OSIR Codex");
    initialize(&root).expect("Production initialization of private target");
    assert!(protected
        .iter()
        .all(|path| !path.starts_with(&project) && !project.starts_with(path)));
    let coordinator = InstallCoordinator(Arc::new(Inner {
        base_root: root.clone(),
        target: root.join("Codex.app"),
        root,
        transport: ReleaseTransport::new().expect("Production release transport"),
        active: Arc::new(Mutex::new(None)),
    }));
    let report_path = project.join("acceptance-metadata.json");
    let mut evidence = json!({"schema":1,"source":"official","baselineBuild":BASELINE_BUILD,"launchRequested":false,"project":project});
    record(
        &report_path,
        &mut evidence,
        "protected_before",
        serde_json::to_value(&before).unwrap(),
    )
    .unwrap();
    eprintln!(
        "OSIR real updater acceptance metadata: {}",
        report_path.display()
    );
    let outcome = exercise(&coordinator, &baseline, &report_path, &mut evidence).await;
    let after = fingerprint_protected(&protected);
    let protected_unchanged = after.as_ref().is_ok_and(|after| after == &before);
    record(
        &report_path,
        &mut evidence,
        "protected_after",
        json!({"unchanged":protected_unchanged,"fingerprints":after.as_ref().ok()}),
    )
    .unwrap();
    record(&report_path, &mut evidence, "finished", json!({"passed":outcome.is_ok() && protected_unchanged,"failureStage":outcome.as_ref().err()})).unwrap();
    assert!(
        protected_unchanged,
        "A protected installation fingerprint changed; see metadata report"
    );
    outcome.expect("Official updater acceptance failed; see metadata report");
}

// First-install package acceptance uses production transport, fixed mirror
// policy, both verifiers and the empty-target transaction. Global discovery is
// recorded separately: a same/newer conversation host can correctly prevent the
// GUI coordinator from installing. Never launch a duplicate of that host.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial]
#[ignore = "Real OSIR mirror Codex first install; requires explicit OSIR_REAL_CODEX_INSTALLER_ACCEPTANCE=1"]
async fn osir_mirror_package_first_install_in_new_private_root() {
    assert_eq!(std::env::var(OPT_IN).as_deref(), Ok("1"));
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap();
    let dev_home = workspace.join(".osir-dev-home");
    owned_dir(&dev_home, false).expect("Existing canonical isolated development home");
    let detection = crate::services::osir::codex_desktop::detect();
    assert!(detection.comparison_complete);
    let mut protected: BTreeSet<PathBuf> = detection
        .installations
        .iter()
        .map(|installation| PathBuf::from(&installation.path).canonicalize().unwrap())
        .collect();
    assert!(protected.contains(Path::new("/Applications/ChatGPT.app")));
    let managed = dev_home.join("Applications/OSIR Codex/Codex.app");
    if managed.exists() {
        assert_eq!(managed.canonicalize().unwrap(), managed);
        protected.insert(managed);
    }
    let before = fingerprint_protected(&protected).expect("Protected installation fingerprints");
    let container = dev_home.join(".installer-acceptance");
    match fs::DirBuilder::new().mode(0o700).create(&container) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => panic!("Cannot create isolated acceptance container"),
    }
    owned_dir(&container, true).expect("Private acceptance container");
    let project = container.join(uuid::Uuid::new_v4().to_string());
    create_private(&project).expect("Fresh acceptance project");
    let root = project.join("Applications/OSIR Codex");
    initialize(&root).expect("Production initialization of empty private target");
    assert!(protected
        .iter()
        .all(|path| !path.starts_with(&project) && !project.starts_with(path)));
    let coordinator = InstallCoordinator(Arc::new(Inner {
        base_root: root.clone(),
        target: root.join("Codex.app"),
        root,
        transport: ReleaseTransport::new().expect("Production release transport"),
        active: Arc::new(Mutex::new(None)),
    }));
    let report_path = project.join("acceptance-metadata.json");
    let mut evidence = json!({"schema":1,"source":"osir_mirror","firstInstall":true,"acceptance":"package_and_empty_target_transaction","launchRequested":false,"project":project});
    record(
        &report_path,
        &mut evidence,
        "protected_before",
        serde_json::to_value(&before).unwrap(),
    )
    .unwrap();
    eprintln!(
        "OSIR mirror first-install metadata: {}",
        report_path.display()
    );
    let outcome: AcceptanceResult<()> = async {
        let initial = coordinator
            .local(LocalAction::Inspect)
            .await
            .map_err(|_| "empty_inspection_failed")?;
        require(
            initial.installed_build.is_none()
                && initial.restore_points.is_empty()
                && !initial.recovery_required,
            "target_not_empty",
        )?;
        record(
            &report_path,
            &mut evidence,
            "empty_inspected",
            serde_json::to_value(&initial).unwrap(),
        )?;
        let checked = coordinator
            .check(Source::OsirMirror)
            .await
            .map_err(|_| "mirror_check_failed")?;
        record(
            &report_path,
            &mut evidence,
            "mirror_checked",
            serde_json::to_value(&checked).unwrap(),
        )?;
        require(
            checked.installed_build.is_none()
                && checked.comparison_complete
                && checked.release.full_size > 0,
            "mirror_first_install_unavailable",
        )?;
        // A real empty-machine baseline (0) is used only for package selection.
        // Production global comparison remains unchanged, including the check
        // result above. The explicit transaction below only writes this UUID.
        let lease = coordinator.lease().map_err(|_| "acceptance_lease_failed")?;
        let plan = coordinator
            .0
            .transport
            .fetch_release(Source::OsirMirror, host_arch(), 0, &lease.token)
            .await
            .map_err(|_| "mirror_plan_failed")?;
        let candidate = plan.metadata().build;
        require(candidate == checked.release.build, "mirror_release_changed")?;
        record(
            &report_path,
            &mut evidence,
            "mirror_install_started",
            json!({"build":candidate,"bytes":checked.release.full_size}),
        )?;
        let result = async {
            let verified = coordinator
                .0
                .transport
                .download(plan, &coordinator.0.root.join("downloads"), &lease.token)
                .await?;
            tokio::task::spawn_blocking(move || {
                let root = lease.inner.root.clone();
                let mut backend = FsBackend::open(&root)?;
                if installed(&root)?.is_some() {
                    return Err(InstallError::Changed);
                }
                not_running(&root.join("Codex.app"))?;
                let mut journal = backend.prepare(&lease.id, None, verified.build())?;
                let work = journal.staged.parent().ok_or(InstallError::Storage)?;
                unpack(&verified, work, &journal.staged, &lease.token)?;
                SystemVerifier.verify(&journal.staged, journal.target_build)?;
                lease.commit()?;
                tx::begin(&mut backend, &SystemVerifier, &root, &mut journal)?;
                Ok::<_, InstallError>(report(&journal))
            })
            .await
            .map_err(|_| InstallError::Storage)?
        }
        .await;
        let installed = match result {
            Ok(installed) => installed,
            Err(error) => {
                record(
                    &report_path,
                    &mut evidence,
                    "mirror_install_failed",
                    install_error_metadata(&error),
                )?;
                return Err("mirror_install_failed");
            }
        };
        record(
            &report_path,
            &mut evidence,
            "mirror_installed",
            serde_json::to_value(&installed).unwrap(),
        )?;
        require(
            installed.step == Step::Completed && installed.build == Some(candidate),
            "mirror_install_not_completed",
        )?;
        let local = coordinator
            .local(LocalAction::Inspect)
            .await
            .map_err(|_| "installed_inspection_failed")?;
        let fingerprint = fingerprint_bundle(&coordinator.0.root.join("Codex.app"))?;
        require(
            local.installed_build == Some(candidate)
                && local.restore_points.is_empty()
                && !local.recovery_required
                && fingerprint.build == candidate,
            "mirror_installed_state_mismatch",
        )?;
        record(
            &report_path,
            &mut evidence,
            "installed_inspected",
            json!({"local":local,"fingerprint":fingerprint}),
        )
    }
    .await;
    let after = fingerprint_protected(&protected);
    let unchanged = after.as_ref().is_ok_and(|after| after == &before);
    record(
        &report_path,
        &mut evidence,
        "protected_after",
        json!({"unchanged":unchanged,"fingerprints":after.as_ref().ok()}),
    )
    .unwrap();
    record(
        &report_path,
        &mut evidence,
        "finished",
        json!({"passed":outcome.is_ok() && unchanged,"failureStage":outcome.as_ref().err()}),
    )
    .unwrap();
    assert!(unchanged, "A protected installation fingerprint changed");
    outcome.expect("OSIR mirror first-install acceptance failed; see metadata report");
}
