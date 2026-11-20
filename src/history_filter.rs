use serde::{Deserialize, Serialize};

use crate::history_db::Entry;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
pub enum ExitFilter {
    #[default]
    All,
    Success,
    Failure,
}

impl ExitFilter {
    pub const ALL: [Self; 3] = [Self::All, Self::Success, Self::Failure];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "Any exit code",
            Self::Success => "Succeeded",
            Self::Failure => "Failed",
        }
    }

    /// atuin writes -1 when it did not record the exit code, so that is neither success nor failure.
    fn accepts(self, exit: i64) -> bool {
        match self {
            Self::All => true,
            Self::Success => exit == 0,
            Self::Failure => exit != 0 && exit != -1,
        }
    }
}

#[derive(Clone, PartialEq, Default, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    pub query: String,
    pub cwd: String,
    pub host: Option<String>,
    pub session: Option<String>,
    pub exit: ExitFilter,
}

impl Filter {
    /// Returns the indices of the matching entries. Each word of the query must occur in the command, ignoring case.
    pub fn apply(&self, entries: &[Entry]) -> Vec<usize> {
        let terms: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let cwd = self.cwd.trim().to_lowercase();
        entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                self.exit.accepts(entry.exit)
                    && self.host.as_deref().is_none_or(|host| entry.host() == host)
                    && self
                        .session
                        .as_deref()
                        .is_none_or(|session| entry.session == session)
                    && terms
                        .iter()
                        .all(|term| entry.command_lower.contains(term.as_str()))
                    && (cwd.is_empty() || entry.cwd.to_lowercase().contains(&cwd))
            })
            .map(|(index, _)| index)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(command: &str, exit: i64, cwd: &str, hostname: &str, session: &str) -> Entry {
        Entry {
            command: command.into(),
            command_lower: command.to_lowercase(),
            exit,
            cwd: cwd.into(),
            hostname: hostname.into(),
            session: session.into(),
            ..Default::default()
        }
    }

    fn entries() -> Vec<Entry> {
        vec![
            entry("git push origin main", 0, "/Users/me/Proj", "mac:me", "s1"),
            entry("cargo build", 101, "/Users/me/proj/sub", "mac:me", "s1"),
            entry("Git status", -1, "/tmp", "box", "s2"),
        ]
    }

    #[test]
    fn empty_filter_keeps_everything() {
        assert_eq!(Filter::default().apply(&entries()), [0, 1, 2]);
    }

    #[test]
    fn every_query_word_must_match_ignoring_case() {
        let filter = |query: &str| {
            Filter {
                query: query.into(),
                ..Default::default()
            }
            .apply(&entries())
        };
        assert_eq!(filter("GIT"), [0, 2]);
        assert_eq!(filter("main git"), [0]);
        assert_eq!(filter("git build"), Vec::<usize>::new());
    }

    #[test]
    fn exit_filter_treats_unrecorded_as_neither() {
        let filter = |exit| {
            Filter {
                exit,
                ..Default::default()
            }
            .apply(&entries())
        };
        assert_eq!(filter(ExitFilter::Success), [0]);
        assert_eq!(filter(ExitFilter::Failure), [1]);
    }

    #[test]
    fn directory_host_and_session_filters() {
        let cwd = Filter {
            cwd: " /users/me/proj ".into(),
            ..Default::default()
        };
        assert_eq!(cwd.apply(&entries()), [0, 1]);
        let host = Filter {
            host: Some("box".into()),
            ..Default::default()
        };
        assert_eq!(host.apply(&entries()), [2]);
        let session = Filter {
            session: Some("s1".into()),
            ..Default::default()
        };
        assert_eq!(session.apply(&entries()), [0, 1]);
    }
}
