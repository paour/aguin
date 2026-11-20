use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Opens without `immutable=1` so that SQLite still reads the rows atuin keeps in the WAL file.
pub fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.pragma_update(None, "query_only", true)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}

/// Changes whenever another connection (atuin) commits to the database.
pub fn data_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.pragma_query_value(None, "data_version", |row| row.get(0))
}

/// atuin's data directory: `$XDG_DATA_HOME/atuin`, else `~/.local/share/atuin`, on macOS too.
pub fn data_dir(home: &Path) -> PathBuf {
    env_path("XDG_DATA_HOME")
        .unwrap_or_else(|| home.join(".local/share"))
        .join("atuin")
}

pub fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "wal").unwrap();
        writer
            .execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
        (dir, path, writer)
    }

    #[test]
    fn connection_refuses_writes() {
        let (_dir, path, _writer) = fixture();
        let conn = open_read_only(&path).unwrap();
        assert!(conn.execute("DELETE FROM t", []).is_err());
    }

    #[test]
    fn sees_uncheckpointed_writes_and_detects_them() {
        let (_dir, path, writer) = fixture();
        let conn = open_read_only(&path).unwrap();
        let before = data_version(&conn).unwrap();
        writer.execute("INSERT INTO t VALUES (2)", []).unwrap();
        assert_ne!(data_version(&conn).unwrap(), before);
        let count: i64 = conn
            .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn missing_file_is_an_error_not_a_new_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.db");
        assert!(open_read_only(&path).is_err());
        assert!(!path.exists());
    }
}
