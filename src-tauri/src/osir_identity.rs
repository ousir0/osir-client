//! OSIR-owned storage names and one-way, non-destructive compatibility copies.
//! Old CC Switch files remain available to the old app and for manual recovery.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use crate::error::AppError;

pub(crate) const DIRECTORY_NAME: &str = ".osir_client";
pub(crate) const DATABASE_NAME: &str = "osir_client.db";
const LEGACY_DATABASE_NAME: &str = "cc-switch.db";

pub(crate) fn product_name() -> &'static str {
    if crate::distribution::is_osir() {
        "OSIR Client"
    } else {
        "CC Switch"
    }
}

pub(crate) fn database_name() -> &'static str {
    if crate::distribution::is_osir() {
        DATABASE_NAME
    } else {
        LEGACY_DATABASE_NAME
    }
}

/// Call before settings caches, the logger and database are opened. The
/// distribution initializer must have selected the channel and isolated HOME.
pub(crate) fn initialize_storage() -> Result<(), AppError> {
    if !crate::distribution::is_osir() {
        return Ok(());
    }
    let legacy = crate::config::get_legacy_device_config_dir();
    let target = crate::config::get_device_config_dir();
    migrate_directory(&legacy, &target)?;
    // A user-selected directory keeps its location; only the app DB name moves.
    migrate_database_in_directory(&crate::config::get_app_config_dir())?;
    Ok(())
}

fn migration_error(message: impl AsRef<str>) -> AppError {
    AppError::Config(format!(
        "OSIR Client 配置迁移未完成，旧配置已保留；请关闭旧版客户端后重试：{}",
        message.as_ref()
    ))
}

#[derive(Debug, PartialEq, Eq)]
struct EntryStamp {
    len: u64,
    modified: Option<SystemTime>,
    link: Option<PathBuf>,
    directory: bool,
}

fn stamp(path: &Path) -> Result<EntryStamp, AppError> {
    let metadata = fs::symlink_metadata(path).map_err(|e| AppError::io(path, e))?;
    Ok(EntryStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        link: if metadata.file_type().is_symlink() {
            Some(fs::read_link(path).map_err(|e| AppError::io(path, e))?)
        } else {
            None
        },
        directory: metadata.is_dir(),
    })
}

fn database_entry(name: &str) -> bool {
    [
        LEGACY_DATABASE_NAME,
        "cc-switch.db-wal",
        "cc-switch.db-shm",
        "cc-switch.db-journal",
        DATABASE_NAME,
        "osir_client.db-wal",
        "osir_client.db-shm",
        "osir_client.db-journal",
    ]
    .contains(&name)
}

fn source_database(directory: &Path) -> PathBuf {
    let osir = directory.join(DATABASE_NAME);
    if osir.exists() {
        osir
    } else {
        directory.join(LEGACY_DATABASE_NAME)
    }
}

fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, EntryStamp>, AppError> {
    let mut entries = BTreeMap::new();
    let mut queue = vec![(PathBuf::new(), 0usize)];
    while let Some((relative, depth)) = queue.pop() {
        if depth > 64 || entries.len() > 100_000 {
            return Err(migration_error("配置目录内容过多，请先检查旧配置目录"));
        }
        let directory = root.join(&relative);
        for entry in fs::read_dir(&directory).map_err(|e| AppError::io(&directory, e))? {
            if entries.len() >= 100_000 {
                return Err(migration_error("配置目录内容过多，请先检查旧配置目录"));
            }
            let entry = entry.map_err(|e| AppError::io(&directory, e))?;
            let path = relative.join(entry.file_name());
            if relative.as_os_str().is_empty()
                && database_entry(&entry.file_name().to_string_lossy())
            {
                continue;
            }
            let state = stamp(&root.join(&path))?;
            if state.directory {
                queue.push((path.clone(), depth + 1));
            }
            entries.insert(path, state);
        }
    }
    Ok(entries)
}

fn private_copy(source: &Path, target: &Path) -> Result<(), AppError> {
    fs::copy(source, target).map_err(|e| AppError::io(target, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(source)
            .map_err(|e| AppError::io(source, e))?
            .permissions()
            .mode();
        fs::set_permissions(target, fs::Permissions::from_mode(mode & 0o700))
            .map_err(|e| AppError::io(target, e))?;
    }
    fs::File::open(target)
        .and_then(|file| file.sync_all())
        .map_err(|e| AppError::io(target, e))?;
    Ok(())
}

fn copy_link(source: &Path, target: &Path) -> Result<(), AppError> {
    let link = fs::read_link(source).map_err(|e| AppError::io(source, e))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(link, target).map_err(|e| AppError::io(target, e))?;
    #[cfg(windows)]
    {
        if source.is_dir() {
            std::os::windows::fs::symlink_dir(link, target)
        } else {
            std::os::windows::fs::symlink_file(link, target)
        }
        .map_err(|e| AppError::io(target, e))?;
    }
    Ok(())
}

fn sqlite_snapshot(source: &Path, target: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(source).map_err(|e| AppError::io(source, e))?;
    if !metadata.file_type().is_file() {
        return Err(migration_error("旧数据库不是普通文件"));
    }
    let source_conn = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| migration_error("无法只读打开旧数据库"))?;
    source_conn
        .busy_timeout(std::time::Duration::from_millis(500))
        .map_err(|_| migration_error("无法检查旧数据库"))?;
    let mut target_conn =
        Connection::open(target).map_err(|_| migration_error("无法创建数据库迁移副本"))?;
    {
        let backup = Backup::new(&source_conn, &mut target_conn)
            .map_err(|_| migration_error("无法准备数据库快照"))?;
        if !matches!(backup.step(-1), Ok(StepResult::Done)) {
            return Err(migration_error("旧数据库正在使用或无法完整读取"));
        }
    }
    let valid: String = target_conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|_| migration_error("数据库迁移副本校验失败"))?;
    if valid != "ok" {
        return Err(migration_error("数据库迁移副本不完整"));
    }
    target_conn
        .close()
        .map_err(|_| migration_error("无法完成数据库迁移副本"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(target, fs::Permissions::from_mode(0o600))
            .map_err(|e| AppError::io(target, e))?;
    }
    fs::File::open(target)
        .and_then(|file| file.sync_all())
        .map_err(|e| AppError::io(target, e))?;
    Ok(())
}

/// Atomically publish without replacing even an empty target directory.
fn publish_directory(source: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let source = CString::new(source.as_os_str().as_bytes())?;
        let target = CString::new(target.as_os_str().as_bytes())?;
        #[cfg(target_os = "macos")]
        let result =
            unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // Windows rename does not replace an existing directory.
        fs::rename(source, target)
    }
}

fn migrate_directory(legacy: &Path, target: &Path) -> Result<(), AppError> {
    if !legacy.exists() {
        return Ok(());
    }
    if target.exists() {
        // Existing OSIR data always wins. A partial destination without a DB
        // must never make an existing legacy DB appear to have disappeared.
        if source_database(legacy).exists() && !target.join(DATABASE_NAME).exists() {
            return Err(migration_error(
                "新配置目录已存在但没有数据库，请先备份并检查该目录",
            ));
        }
        return Ok(());
    }
    let metadata = fs::symlink_metadata(legacy).map_err(|e| AppError::io(legacy, e))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(migration_error(
            "旧配置目录不是普通目录，请先确认自定义存储位置",
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| migration_error("无法确定配置目录"))?;
    fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    let stage = tempfile::Builder::new()
        .prefix(".osir_client-migration-")
        .tempdir_in(parent)
        .map_err(|e| AppError::io(parent, e))?;
    let before = inventory(legacy)?;
    for (relative, state) in &before {
        let source = legacy.join(relative);
        let destination = stage.path().join(relative);
        if state.directory {
            fs::create_dir_all(&destination).map_err(|e| AppError::io(&destination, e))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&destination, fs::Permissions::from_mode(0o700))
                    .map_err(|e| AppError::io(&destination, e))?;
            }
        } else if state.link.is_some() {
            copy_link(&source, &destination)?;
        } else if fs::symlink_metadata(&source)
            .map_err(|e| AppError::io(&source, e))?
            .is_file()
        {
            private_copy(&source, &destination)?;
        } else {
            return Err(migration_error("旧配置包含无法迁移的特殊文件"));
        }
    }
    let database = source_database(legacy);
    if database.exists() {
        sqlite_snapshot(&database, &stage.path().join(DATABASE_NAME))?;
    }
    if before != inventory(legacy)? {
        return Err(migration_error("旧配置在迁移期间发生变化"));
    }
    publish_directory(stage.path(), target).map_err(|e| AppError::io(target, e))?;
    Ok(())
}

fn migrate_database_in_directory(directory: &Path) -> Result<(), AppError> {
    let source = directory.join(LEGACY_DATABASE_NAME);
    let target = directory.join(DATABASE_NAME);
    if target.exists() || !source.exists() {
        return Ok(());
    }
    let stage = tempfile::Builder::new()
        .prefix(".osir_client-db-migration-")
        .tempfile_in(directory)
        .map_err(|e| AppError::io(directory, e))?
        .into_temp_path();
    sqlite_snapshot(&source, &stage)?;
    stage
        .persist_noclobber(&target)
        .map_err(|e| AppError::io(&target, e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(root: &Path) -> Connection {
        fs::create_dir_all(root).unwrap();
        let conn = Connection::open(root.join(LEGACY_DATABASE_NAME)).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE fixture(value TEXT); INSERT INTO fixture VALUES ('initial');").unwrap();
        conn
    }

    #[test]
    fn osir_migration_copies_committed_wal_and_keeps_source_and_private_settings() {
        let home = tempfile::tempdir().unwrap();
        let legacy = home.path().join(".cc-switch");
        let target = home.path().join(DIRECTORY_NAME);
        let conn = seed(&legacy);
        conn.execute("INSERT INTO fixture VALUES ('wal-row')", [])
            .unwrap();
        fs::write(legacy.join("settings.json"), b"{\"synthetic\":true}").unwrap();
        migrate_directory(&legacy, &target).unwrap();
        assert!(legacy.join(LEGACY_DATABASE_NAME).exists());
        assert!(!target.join(LEGACY_DATABASE_NAME).exists());
        assert!(!target.join("cc-switch.db-wal").exists());
        let migrated = Connection::open(target.join(DATABASE_NAME)).unwrap();
        assert_eq!(
            migrated
                .query_row("SELECT COUNT(*) FROM fixture", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            fs::read(legacy.join("settings.json")).unwrap(),
            fs::read(target.join("settings.json")).unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(target.join("settings.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
        // Repeating startup must not roll back a newer OSIR write.
        migrated
            .execute("INSERT INTO fixture VALUES ('osir-write')", [])
            .unwrap();
        migrate_directory(&legacy, &target).unwrap();
        assert_eq!(
            migrated
                .query_row("SELECT COUNT(*) FROM fixture", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
    }

    #[test]
    fn invalid_legacy_database_does_not_publish_an_empty_destination() {
        let home = tempfile::tempdir().unwrap();
        let legacy = home.path().join("legacy");
        let target = home.path().join("target");
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join(LEGACY_DATABASE_NAME), b"invalid-synthetic-db").unwrap();
        assert!(migrate_directory(&legacy, &target).is_err());
        assert!(!target.exists());
        assert_eq!(
            fs::read(legacy.join(LEGACY_DATABASE_NAME)).unwrap(),
            b"invalid-synthetic-db"
        );
    }

    #[test]
    fn custom_directory_db_migration_is_noclobber_and_non_destructive() {
        let root = tempfile::tempdir().unwrap();
        let legacy = seed(root.path());
        migrate_database_in_directory(root.path()).unwrap();
        let migrated = Connection::open(root.path().join(DATABASE_NAME)).unwrap();
        migrated
            .execute("INSERT INTO fixture VALUES ('new-write')", [])
            .unwrap();
        migrate_database_in_directory(root.path()).unwrap();
        assert_eq!(
            legacy
                .query_row("SELECT COUNT(*) FROM fixture", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            migrated
                .query_row("SELECT COUNT(*) FROM fixture", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn an_existing_destination_is_never_replaced_even_if_empty() {
        let home = tempfile::tempdir().unwrap();
        let legacy = home.path().join("legacy");
        let target = home.path().join("target");
        let _conn = seed(&legacy);
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep.txt"), b"keep").unwrap();
        assert!(migrate_directory(&legacy, &target).is_err());
        assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep");
        let stage = tempfile::tempdir_in(home.path()).unwrap();
        assert!(publish_directory(stage.path(), &target).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn skill_symlinks_are_preserved_without_copying_external_files() {
        let home = tempfile::tempdir().unwrap();
        let legacy = home.path().join("legacy");
        let target = home.path().join("target");
        fs::create_dir_all(legacy.join("skills")).unwrap();
        let external = home.path().join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("SKILL.md"), b"synthetic").unwrap();
        std::os::unix::fs::symlink(&external, legacy.join("skills/reference")).unwrap();
        migrate_directory(&legacy, &target).unwrap();
        assert_eq!(
            fs::read_link(target.join("skills/reference")).unwrap(),
            external
        );
        assert!(fs::symlink_metadata(target.join("skills/reference"))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
