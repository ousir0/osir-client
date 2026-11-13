//! 数据库模块 - SQLite 数据持久化
//!
//! 此模块提供应用的核心数据存储功能，包括：
//! - 供应商配置管理
//! - MCP 服务器配置
//! - 提示词管理
//! - Skills 管理
//! - 通用设置存储
//!
//! ## 架构设计
//!
//! ```text
//! database/
//! ├── mod.rs        - Database 结构体 + 初始化
//! ├── schema.rs     - 表结构定义 + Schema 迁移
//! ├── backup.rs     - SQL 导入导出 + 快照备份
//! ├── migration.rs  - JSON → SQLite 数据迁移
//! └── dao/          - 数据访问对象
//!     ├── providers.rs
//!     ├── mcp.rs
//!     ├── prompts.rs
//!     ├── skills.rs
//!     └── settings.rs
//! ```

pub(crate) mod backup;
mod compatibility;
mod dao;
mod migration;
mod schema;

#[cfg(test)]
mod tests;

// DAO 类型导出供外部使用
pub use dao::custom_icons::{CustomIconEntry, CUSTOM_ICONS_KEY};
pub(crate) use dao::providers_seed::{
    is_official_seed_id, CLAUDE_DESKTOP_OFFICIAL_PROVIDER_ID, CODEX_OFFICIAL_PROVIDER_ID,
    GROKBUILD_OFFICIAL_PROVIDER_ID,
};
pub(crate) use dao::proxy::{PRICING_SOURCE_REQUEST, PRICING_SOURCE_RESPONSE};
pub use dao::FailoverQueueItem;
pub use dao::Profile;

use crate::error::AppError;
use rusqlite::{hooks::Action, Connection};
use serde::Serialize;
use std::sync::Mutex;

// DAO 方法通过 impl Database 提供，无需额外导出

/// Reviewed shared schema ceiling. OSIR-specific revisions are tracked
/// separately in compatibility.rs because the migration histories diverged.
pub(crate) const SCHEMA_VERSION: i32 = 26;

/// 安全地序列化 JSON，避免 unwrap panic
pub(crate) fn to_json_string<T: Serialize>(value: &T) -> Result<String, AppError> {
    serde_json::to_string(value)
        .map_err(|e| AppError::Config(format!("JSON serialization failed: {e}")))
}

/// 安全地获取 Mutex 锁，避免 unwrap panic
macro_rules! lock_conn {
    ($mutex:expr) => {
        $mutex
            .lock()
            .map_err(|e| AppError::Database(format!("Mutex lock failed: {}", e)))?
    };
}

// 导出宏供子模块使用
pub(crate) use lock_conn;

/// 数据库连接封装
///
/// 使用 Mutex 包装 Connection 以支持在多线程环境（如 Tauri State）中共享。
/// rusqlite::Connection 本身不是 Sync 的，因此需要这层包装。
pub struct Database {
    pub(crate) conn: Mutex<Connection>,
    /// 请求日志总数缓存，见 `services::usage_stats::LogCountCache`
    pub(crate) log_count_cache: Mutex<Option<crate::services::usage_stats::LogCountCache>>,
}

fn register_db_change_hook(conn: &Connection) {
    conn.update_hook(Some(
        |action: Action, _database: &str, table: &str, _row_id: i64| match action {
            Action::SQLITE_INSERT | Action::SQLITE_UPDATE | Action::SQLITE_DELETE => {
                crate::services::webdav_auto_sync::notify_db_changed(table);
                crate::services::s3_auto_sync::notify_db_changed(table);
            }
            _ => {}
        },
    ));
}

impl Database {
    /// 初始化数据库连接并创建表
    ///
    /// OSIR 数据库默认位于 `~/.osir_client/osir_client.db`。
    pub fn init() -> Result<Self, AppError> {
        crate::osir_identity::initialize_storage()?;
        let db_path = crate::config::get_database_path();
        Self::prepare_database_file(&db_path)?;

        let mut conn = Connection::open(&db_path).map_err(|e| AppError::Database(e.to_string()))?;

        // Inspect and back up the original schema before any DDL or persistent
        // PRAGMA. In particular, v0 with user tables is a legacy DB, not a new DB.
        Self::prepare_schema_upgrade(&conn, || {
            let guard = backup::lock_backup_file_operations()?;
            Self::backup_database_file_from_conn(&guard, &conn, &[])
        })?;
        if Self::schema_needs_upgrade(&conn)? {
            Self::upgrade_schema_from_snapshot(&mut conn)?;
        } else {
            Self::initialize_schema_on_conn(&conn)?;
        }

        // 启用外键约束
        conn.execute("PRAGMA foreign_keys = ON;", [])
            .map_err(|e| AppError::Database(e.to_string()))?;
        register_db_change_hook(&conn);

        let db = Self {
            conn: Mutex::new(conn),
            log_count_cache: Mutex::new(None),
        };
        if let Err(e) = db.ensure_incremental_auto_vacuum() {
            log::warn!("Failed to ensure incremental auto-vacuum: {e}");
        }
        db.ensure_model_pricing_seeded()?;
        if let Err(e) = crate::services::model_pricing::sync_local_model_pricing(&db) {
            log::warn!("Failed to sync local model pricing file: {e}");
        }

        // Startup cleanup: prune old logs and reclaim space
        if let Err(e) = db.cleanup_old_stream_check_logs(7) {
            log::warn!("Startup stream_check_logs cleanup failed: {e}");
        }
        if let Err(e) = db.rollup_and_prune(30) {
            log::warn!("Startup rollup_and_prune failed: {e}");
        }
        // Reclaim disk space after cleanup
        {
            let conn = lock_conn!(db.conn);
            if let Err(e) = Self::incremental_vacuum_on_conn(&conn) {
                log::warn!("Startup incremental vacuum failed: {e}");
            }
        }

        Ok(db)
    }

    /// Create an empty file privately, never replace a live SQLite inode.
    /// Existing user-selected parent directories keep their permissions.
    fn prepare_database_file(path: &std::path::Path) -> Result<(), AppError> {
        if let Some(parent) = path.parent() {
            Self::create_database_directory(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(AppError::io(path, e)),
        }
        Self::protect_database_file(path)?;
        // SQLite inherits the main DB mode for newly created sidecars on Unix.
        // Tighten existing sidecars in place before opening the connection too.
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            let sidecar = std::path::PathBuf::from(name);
            match Self::protect_database_file(&sidecar) {
                Err(AppError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound => {}
                result => result?,
            }
        }
        Ok(())
    }

    pub(super) fn create_database_directory(path: &std::path::Path) -> Result<(), AppError> {
        crate::config::create_private_database_directory(path)
    }

    fn protect_database_file(path: &std::path::Path) -> Result<(), AppError> {
        // Reject links instead of chmod'ing an unrelated file. No recursive
        // permission changes and no atomic-write/rename of the live database.
        let meta = std::fs::symlink_metadata(path).map_err(|e| AppError::io(path, e))?;
        if !meta.is_file() {
            return Err(AppError::Database(
                "数据库文件路径不安全，请检查文件位置后重试。".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
                .map_err(|e| AppError::io(path, e))?;
            let meta = file.metadata().map_err(|e| AppError::io(path, e))?;
            if !meta.is_file() || meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() } {
                return Err(AppError::Database(
                    "数据库文件路径不安全，请检查文件位置后重试。".into(),
                ));
            }
            file.set_permissions(std::fs::Permissions::from_mode(meta.mode() & 0o600))
                .map_err(|e| AppError::io(path, e))?;
        }
        #[cfg(windows)]
        crate::config::protect_private_database_file(path)?;
        Ok(())
    }

    /// The injected operation is the existing validated SQLite Backup engine.
    /// Keeping this gate separate makes failures testable without writing HOME.
    fn prepare_schema_upgrade<F>(conn: &Connection, backup: F) -> Result<(), AppError>
    where
        F: FnOnce() -> Result<Option<std::path::PathBuf>, AppError>,
    {
        Self::validate_schema_before_writes(conn)?;
        let version = Self::get_user_version(conn)?;
        if Self::schema_needs_upgrade(conn)? && Self::has_user_tables(conn)? {
            log::info!("Creating pre-migration database backup (v{version} → v{SCHEMA_VERSION})");
            // Do not include raw backup errors: they may contain SQL or data.
            let fail = || {
                AppError::Database(
                "升级前备份未完成，数据库尚未升级。请检查备份目录的写入权限和可用磁盘空间后重试；如需恢复，请保留原数据库和已有备份。".into(),
            )
            };
            backup().map_err(|_| fail())?.ok_or_else(fail)?;
        }
        Ok(())
    }

    /// 读取磁盘上数据库的 `user_version`；仅当它比应用支持的 [`SCHEMA_VERSION`]
    /// 更新时返回 `Some(version)`。
    ///
    /// 用于初始化失败后判断是否为「数据库版本过新（应用过旧，需升级应用）」的可恢复
    /// 场景——此时不应反复弹出无效的重试对话框，而应引导用户在应用内升级。
    pub fn stored_user_version_exceeds_supported(
        db_path: &std::path::Path,
    ) -> Result<Option<i32>, AppError> {
        if !db_path.exists() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| AppError::Database(e.to_string()))?;
        let version = Self::get_user_version(&conn)?;
        Ok((version > SCHEMA_VERSION).then_some(version))
    }

    /// 创建内存数据库（用于测试）
    pub fn memory() -> Result<Self, AppError> {
        let conn = Connection::open_in_memory().map_err(|e| AppError::Database(e.to_string()))?;

        // 启用外键约束
        conn.execute("PRAGMA foreign_keys = ON;", [])
            .map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute("PRAGMA auto_vacuum = INCREMENTAL;", [])
            .map_err(|e| AppError::Database(e.to_string()))?;
        register_db_change_hook(&conn);

        let db = Self {
            conn: Mutex::new(conn),
            log_count_cache: Mutex::new(None),
        };
        db.create_tables()?;
        db.ensure_model_pricing_seeded()?;

        Ok(db)
    }

    pub(crate) fn get_auto_vacuum_mode(conn: &Connection) -> Result<i32, AppError> {
        conn.query_row("PRAGMA auto_vacuum;", [], |row| row.get(0))
            .map_err(|e| AppError::Database(format!("读取 auto_vacuum 失败: {e}")))
    }

    /// 回收全部空闲页。`PRAGMA incremental_vacuum` 每释放一页产出一行结果，
    /// 必须把结果读完才会回收完；`execute_batch` 只 step 一次，只能回收 1 页。
    pub(crate) fn incremental_vacuum_on_conn(conn: &Connection) -> Result<(), AppError> {
        let mut stmt = conn
            .prepare("PRAGMA incremental_vacuum;")
            .map_err(|e| AppError::Database(format!("执行 incremental_vacuum 失败: {e}")))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| AppError::Database(format!("执行 incremental_vacuum 失败: {e}")))?;
        while rows
            .next()
            .map_err(|e| AppError::Database(format!("执行 incremental_vacuum 失败: {e}")))?
            .is_some()
        {}
        Ok(())
    }

    fn has_user_tables(conn: &Connection) -> Result<bool, AppError> {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(format!("读取表数量失败: {e}")))?;
        Ok(count > 0)
    }

    pub(crate) fn ensure_incremental_auto_vacuum_on_conn(
        conn: &Connection,
    ) -> Result<bool, AppError> {
        let mode = Self::get_auto_vacuum_mode(conn)?;
        if mode == 2 {
            return Ok(false);
        }

        let has_tables = Self::has_user_tables(conn)?;
        conn.execute("PRAGMA auto_vacuum = INCREMENTAL;", [])
            .map_err(|e| AppError::Database(format!("设置 auto_vacuum 失败: {e}")))?;

        if !has_tables {
            return Ok(false);
        }

        conn.execute("VACUUM;", [])
            .map_err(|e| AppError::Database(format!("执行 VACUUM 失败: {e}")))?;
        conn.execute("PRAGMA foreign_keys = ON;", [])
            .map_err(|e| AppError::Database(format!("恢复 foreign_keys 失败: {e}")))?;
        Ok(true)
    }

    pub(crate) fn ensure_incremental_auto_vacuum(&self) -> Result<bool, AppError> {
        let mode = {
            let conn = lock_conn!(self.conn);
            Self::get_auto_vacuum_mode(&conn)?
        };
        if mode == 2 {
            return Ok(false);
        }

        let has_tables = {
            let conn = lock_conn!(self.conn);
            Self::has_user_tables(&conn)?
        };
        if has_tables {
            log::info!(
                "Detected auto_vacuum={mode}, rebuilding database to enable incremental vacuum"
            );
            self.backup_database_file()?;
        }

        let rebuilt = {
            let conn = lock_conn!(self.conn);
            Self::ensure_incremental_auto_vacuum_on_conn(&conn)?
        };

        if rebuilt {
            log::info!("Incremental auto-vacuum enabled after database rebuild");
        } else {
            log::info!("Incremental auto-vacuum configured for new database");
        }

        Ok(rebuilt)
    }

    /// 检查 MCP 服务器表是否为空
    pub fn is_mcp_table_empty(&self) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM mcp_servers", [], |row| row.get(0))
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(count == 0)
    }

    /// 检查提示词表是否为空
    pub fn is_prompts_table_empty(&self) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(count == 0)
    }
}
