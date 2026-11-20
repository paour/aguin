use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use rusqlite::Connection;

use crate::sqlite::{self, POLL_INTERVAL};

#[derive(Default)]
pub struct Entry {
    pub id: String,
    /// Nanoseconds since the Unix epoch, UTC.
    pub timestamp: i64,
    /// Nanoseconds, or -1 when atuin did not record it.
    pub duration: i64,
    /// -1 when atuin did not record it.
    pub exit: i64,
    pub command: String,
    pub command_lower: String,
    pub cwd: String,
    pub session: String,
    /// atuin writes `host:user`, but some older rows have only the host.
    pub hostname: String,
}

impl Entry {
    pub fn host(&self) -> &str {
        self.hostname
            .split_once(':')
            .map_or(&self.hostname, |(host, _)| host)
    }

    pub fn user(&self) -> Option<&str> {
        self.hostname.split_once(':').map(|(_, user)| user)
    }
}

pub enum Update {
    Loaded(Vec<Entry>),
    Failed(String),
}

/// Resolves the database path the same way atuin does: `db_path` in config.toml, else the data dir.
pub fn default_path(home: &Path) -> PathBuf {
    let config_dir = sqlite::env_path("ATUIN_CONFIG_DIR")
        .or_else(|| sqlite::env_path("XDG_CONFIG_HOME").map(|dir| dir.join("atuin")))
        .unwrap_or_else(|| home.join(".config/atuin"));
    std::fs::read_to_string(config_dir.join("config.toml"))
        .ok()
        .and_then(|text| db_path_from_config(&text, home))
        .unwrap_or_else(|| sqlite::data_dir(home).join("history.db"))
}

fn db_path_from_config(text: &str, home: &Path) -> Option<PathBuf> {
    let config: toml::Table = toml::from_str(text).ok()?;
    let raw = config.get("db_path")?.as_str()?;
    Some(match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(raw),
    })
}

pub fn load(conn: &Connection) -> rusqlite::Result<Vec<Entry>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, timestamp, duration, exit, command, cwd, session, hostname
         FROM history
         WHERE deleted_at IS NULL
         ORDER BY timestamp DESC",
    )?;
    stmt.query_map([], |row| {
        let command: String = row.get(4)?;
        Ok(Entry {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            duration: row.get(2)?,
            exit: row.get(3)?,
            command_lower: command.to_lowercase(),
            command,
            cwd: row.get(5)?,
            session: row.get(6)?,
            hostname: row.get(7)?,
        })
    })?
    .collect()
}

/// Loads the history on a background thread, then reloads it each time atuin writes to it.
pub fn spawn_loader(path: PathBuf, notify: impl Fn() + Send + 'static) -> Receiver<Update> {
    let (tx, rx) = channel();
    thread::spawn(move || run_loader(&path, &tx, notify));
    rx
}

fn run_loader(path: &Path, tx: &Sender<Update>, notify: impl Fn()) {
    let conn = match sqlite::open_read_only(path) {
        Ok(conn) => conn,
        Err(err) => {
            let _ = tx.send(Update::Failed(format!(
                "Cannot open {}: {err}",
                path.display()
            )));
            notify();
            return;
        }
    };
    let mut loaded_version = None;
    loop {
        let update = match sqlite::data_version(&conn) {
            Ok(version) if Some(version) == loaded_version => None,
            Ok(version) => Some(match load(&conn) {
                Ok(entries) => {
                    loaded_version = Some(version);
                    Update::Loaded(entries)
                }
                Err(err) => Update::Failed(format!("Cannot read history: {err}")),
            }),
            Err(err) => Some(Update::Failed(format!("Cannot read history: {err}"))),
        };
        if let Some(update) = update {
            if tx.send(update).is_err() {
                return;
            }
            notify();
        }
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_skips_deleted_rows_and_sorts_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE history (
                    id text primary key, timestamp integer not null, duration integer not null,
                    exit integer not null, command text not null, cwd text not null,
                    session text not null, hostname text not null, deleted_at integer);
                 INSERT INTO history VALUES ('a', 100, 5, 0, 'ls', '/tmp', 's1', 'mac:me', NULL);
                 INSERT INTO history VALUES ('b', 300, -1, -1, 'Git Status', '/src', 's1', 'mac:me', NULL);
                 INSERT INTO history VALUES ('c', 200, 5, 1, 'rm -rf x', '/tmp', 's2', 'box', 250);",
            )
            .unwrap();
        let entries = load(&sqlite::open_read_only(&path).unwrap()).unwrap();
        let ids: Vec<_> = entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
        assert_eq!(entries[0].command_lower, "git status");
    }

    #[test]
    fn reads_db_path_from_config() {
        let home = Path::new("/home/me");
        assert_eq!(
            db_path_from_config("db_path = \"~/data/h.db\"", home),
            Some(PathBuf::from("/home/me/data/h.db"))
        );
        assert_eq!(
            db_path_from_config("db_path = \"/srv/h.db\"\n[sync]\nrecords = true", home),
            Some(PathBuf::from("/srv/h.db"))
        );
        assert_eq!(db_path_from_config("# db_path = \"/x\"", home), None);
    }

    #[test]
    fn splits_host_and_user() {
        let full = Entry {
            hostname: "mac:me".into(),
            ..Default::default()
        };
        assert_eq!((full.host(), full.user()), ("mac", Some("me")));
        let bare = Entry {
            hostname: "box".into(),
            ..Default::default()
        };
        assert_eq!((bare.host(), bare.user()), ("box", None));
    }
}
