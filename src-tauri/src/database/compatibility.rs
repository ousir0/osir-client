//! The fork's local revision is independent of the shared SQLite user_version.
//! OSIR v24/v25 and upstream v24/v25 are different migration histories.

use super::{Database, SCHEMA_VERSION};
use crate::error::AppError;
use rusqlite::{backup::Backup, Connection, OptionalExtension};

const LOCAL_REVISION_KEY: &str = "osir_schema_revision";
const LOCAL_REVISION: &str = "1";

impl Database {
    fn local_schema_revision(conn: &Connection) -> Result<Option<String>, AppError> {
        if !Self::table_exists(conn, "settings")? {
            return Ok(None);
        }
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [LOCAL_REVISION_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| Self::unsupported_schema())
    }

    fn unsupported_schema() -> AppError {
        AppError::Database(
            "无法确认此数据库是否兼容，尚未修改数据。请保留配置目录和备份，联系支持人员检查。"
                .into(),
        )
    }

    /// Read-only gate, also used by imports before their first schema write.
    pub(super) fn validate_schema_before_writes(conn: &Connection) -> Result<(), AppError> {
        let version = Self::get_user_version(conn)?;
        if version > SCHEMA_VERSION {
            return Err(AppError::Database(format!(
                "数据库版本过新（{version}），当前应用仅支持 {SCHEMA_VERSION}，请升级应用后重试。"
            )));
        }
        if version < 0 {
            return Err(Self::unsupported_schema());
        }
        let local_revision = Self::local_schema_revision(conn)?;
        if let Some(revision) = &local_revision {
            if revision != LOCAL_REVISION || !Self::table_exists(conn, "osir_model_credentials")? {
                return Err(Self::unsupported_schema());
            }
        }
        // Legacy migrations handle sparse, older schemas. The newly accepted
        // v26 must satisfy the entire shared column/primary-key contract before
        // create_tables can fill gaps and conceal an unrelated/truncated DB.
        if version == 26 {
            let baseline = Connection::open_in_memory()?;
            baseline.execute_batch(include_str!("upstream_v26_schema.sql"))?;
            Self::create_osir_model_credentials_table(&baseline)?;
            let tables = {
                let mut stmt = baseline.prepare(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
                )?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            for table in tables {
                if table == "osir_model_credentials" && !Self::table_exists(conn, &table)? {
                    continue;
                }
                let actual = Self::schema_columns(conn, &table)?;
                for (column, primary_key) in Self::schema_columns(&baseline, &table)? {
                    if !actual.contains(&(column, primary_key)) {
                        return Err(Self::unsupported_schema());
                    }
                }
            }
        }
        Ok(())
    }

    fn schema_columns(conn: &Connection, table: &str) -> Result<Vec<(String, i64)>, AppError> {
        let is_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1 AND type='table')",
            [table],
            |row| row.get(0),
        )?;
        if !is_table {
            return Err(Self::unsupported_schema());
        }
        let mut stmt = conn.prepare("SELECT name, pk FROM pragma_table_info(?1)")?;
        let rows = stmt.query_map([table], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| Self::unsupported_schema())
    }

    pub(super) fn schema_needs_upgrade(conn: &Connection) -> Result<bool, AppError> {
        Ok(Self::get_user_version(conn)? < SCHEMA_VERSION
            || Self::local_schema_revision(conn)?.as_deref() != Some(LOCAL_REVISION)
            || !Self::table_exists(conn, "osir_model_credentials")?)
    }

    /// Called inside the migration savepoint, including when user_version is
    /// already 26. It does not overwrite upstream flags or OSIR credentials.
    pub(super) fn reconcile_local_schema(conn: &Connection) -> Result<(), AppError> {
        Self::create_osir_model_credentials_table(conn)?;
        for table in ["mcp_servers", "skills"] {
            if Self::table_exists(conn, table)? {
                for column in ["enabled_dsh", "enabled_copilot", "enabled_workbuddy"] {
                    Self::add_column_if_missing(conn, table, column, "BOOLEAN NOT NULL DEFAULT 0")?;
                }
            }
        }
        if Self::table_exists(conn, "session_log_sync")? {
            Self::add_column_if_missing(conn, "session_log_sync", "parser_state", "TEXT")?;
        }
        conn.execute(
            "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )?;
        conn.execute(
            "INSERT INTO settings(key,value) VALUES (?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [LOCAL_REVISION_KEY, LOCAL_REVISION],
        )?;
        Ok(())
    }

    pub(super) fn initialize_schema_on_conn(conn: &Connection) -> Result<(), AppError> {
        Self::validate_schema_before_writes(conn)?;
        conn.execute_batch("SAVEPOINT initialize_schema")?;
        let result = (|| {
            Self::create_tables_on_conn(conn)?;
            Self::apply_schema_migrations_on_conn(conn)
        })();
        match result {
            Ok(()) => conn
                .execute_batch("RELEASE initialize_schema")
                .map_err(Into::into),
            Err(error) => {
                conn.execute_batch("ROLLBACK TO initialize_schema; RELEASE initialize_schema")?;
                Err(error)
            }
        }
    }

    /// Startup has already retained a verified original backup. Migrate a
    /// private temporary snapshot and validate it before replacing live bytes.
    pub(super) fn upgrade_schema_from_snapshot(conn: &mut Connection) -> Result<(), AppError> {
        let temporary = tempfile::NamedTempFile::new().map_err(|source| AppError::IoContext {
            context: "无法准备数据库升级副本".into(),
            source,
        })?;
        let mut staged = Connection::open(temporary.path())?;
        Self::complete_backup(&Backup::new(conn, &mut staged)?, "准备数据库升级副本")?;
        if !Self::has_user_tables(&staged)? {
            staged.execute_batch("PRAGMA auto_vacuum=INCREMENTAL")?;
        }
        Self::initialize_schema_on_conn(&staged)?;
        // Perform any required vacuum rebuild on the private copy as well,
        // rather than rebuilding live bytes after migration has been applied.
        Self::ensure_incremental_auto_vacuum_on_conn(&staged)?;
        Self::validate_sqlite_integrity(&staged)?;
        Self::validate_schema_before_writes(&staged)?;
        let backup = Backup::new(&staged, conn)?;
        Self::complete_backup(&backup, "应用数据库升级")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_v26_migrates_with_foreign_flags_and_osir_credentials() -> Result<(), AppError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(include_str!("upstream_v26_schema.sql"))?;
        conn.execute(
            "INSERT INTO mcp_servers(id,name,server_config,enabled_dsh,enabled_copilot,enabled_workbuddy)
             VALUES('m','server','{}',1,1,1)",
            [],
        )?;
        conn.execute(
            "INSERT INTO skills(id,name,directory,enabled_dsh,enabled_copilot,enabled_workbuddy)
             VALUES('s','skill','s',1,1,1)",
            [],
        )?;
        Database::initialize_schema_on_conn(&conn)?;
        assert_eq!(Database::get_user_version(&conn)?, 26);
        assert_eq!(
            Database::local_schema_revision(&conn)?.as_deref(),
            Some("1")
        );
        assert!(Database::table_exists(&conn, "osir_model_credentials")?);

        for table in ["mcp_servers", "skills"] {
            let flags: (i64, i64, i64) = conn.query_row(
                &format!("SELECT enabled_dsh,enabled_copilot,enabled_workbuddy FROM {table}"),
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(flags, (1, 1, 1));
        }
        Ok(())
    }

    #[test]
    fn v26_requires_known_structure_before_schema_writes() -> Result<(), AppError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("CREATE TABLE unrelated(value TEXT); PRAGMA user_version=26;")?;
        assert!(Database::initialize_schema_on_conn(&conn).is_err());
        assert!(!Database::table_exists(&conn, "providers")?);
        Ok(())
    }

    #[test]
    fn both_migration_histories_v23_to_v25_reach_same_schema_without_losing_keys(
    ) -> Result<(), AppError> {
        for version in [23, 24, 25] {
            for osir in [false, true] {
                let conn = Connection::open_in_memory()?;
                conn.execute_batch(include_str!("upstream_v26_schema.sql"))?;
                for table in ["mcp_servers", "skills"] {
                    conn.execute_batch(&format!(
                        "ALTER TABLE {table} DROP COLUMN enabled_workbuddy"
                    ))?;
                    if version <= 24 || osir {
                        conn.execute_batch(&format!(
                            "ALTER TABLE {table} DROP COLUMN enabled_copilot"
                        ))?;
                    }
                    if version == 23 || (osir && version == 24) {
                        conn.execute_batch(&format!(
                            "ALTER TABLE {table} DROP COLUMN enabled_dsh"
                        ))?;
                    }
                }
                if osir || version == 23 {
                    conn.execute_batch("ALTER TABLE session_log_sync DROP COLUMN parser_state")?;
                }
                if osir && version >= 24 {
                    Database::create_osir_model_credentials_table(&conn)?;
                    conn.execute(
                        "INSERT INTO osir_model_credentials VALUES('fixture','fixture',X'010203')",
                        [],
                    )?;
                }
                Database::set_user_version(&conn, version)?;
                Database::initialize_schema_on_conn(&conn)?;
                Database::validate_schema_before_writes(&conn)?;
                assert_eq!(Database::get_user_version(&conn)?, 26);
                if osir && version >= 24 {
                    let payload: Vec<u8> = conn.query_row(
                        "SELECT payload FROM osir_model_credentials WHERE account='fixture'",
                        [],
                        |row| row.get(0),
                    )?;
                    assert_eq!(payload, vec![1, 2, 3]);
                }
                assert!(!Database::schema_needs_upgrade(&conn)?);
            }
        }
        Ok(())
    }

    #[test]
    fn known_v26_still_requires_backup_until_local_revision_is_applied() -> Result<(), AppError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(include_str!("upstream_v26_schema.sql"))?;
        let error = Database::prepare_schema_upgrade(&conn, || Ok(None)).unwrap_err();
        assert!(error.to_string().contains("升级前备份未完成"));
        assert!(!Database::table_exists(&conn, "osir_model_credentials")?);
        assert!(Database::local_schema_revision(&conn)?.is_none());
        Database::initialize_schema_on_conn(&conn)?;
        Database::prepare_schema_upgrade(&conn, || panic!("already upgraded"))?;
        Ok(())
    }

    #[test]
    fn failed_staged_upgrade_preserves_original_schema_and_data() -> Result<(), AppError> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch(include_str!("upstream_v26_schema.sql"))?;
        Database::set_user_version(&conn, 25)?;
        conn.execute_batch(
            "INSERT INTO settings VALUES('fixture','original');
             CREATE TRIGGER fail_local_revision BEFORE INSERT ON settings
             WHEN NEW.key='osir_schema_revision' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;",
        )?;
        assert!(Database::upgrade_schema_from_snapshot(&mut conn).is_err());
        assert_eq!(Database::get_user_version(&conn)?, 25);
        assert!(!Database::table_exists(&conn, "osir_model_credentials")?);
        let value: String = conn.query_row(
            "SELECT value FROM settings WHERE key='fixture'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(value, "original");
        Ok(())
    }

    #[test]
    fn invalid_local_revision_missing_local_table_and_future_version_do_not_write(
    ) -> Result<(), AppError> {
        for (version, revision) in [(26, "2"), (26, "1"), (27, "1")] {
            let conn = Connection::open_in_memory()?;
            conn.execute_batch(include_str!("upstream_v26_schema.sql"))?;
            Database::set_user_version(&conn, version)?;
            conn.execute(
                "INSERT INTO settings VALUES('osir_schema_revision',?1)",
                [revision],
            )?;
            assert!(Database::initialize_schema_on_conn(&conn).is_err());
            assert!(!Database::table_exists(&conn, "osir_model_credentials")?);
            assert_eq!(Database::get_user_version(&conn)?, version);
        }
        Ok(())
    }
}
