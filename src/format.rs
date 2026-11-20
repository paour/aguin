use std::borrow::Cow;
use std::fmt::Display;

use chrono::{DateTime, Local, Utc};

pub fn time_from_nanos(nanos: i64, format: &str) -> String {
    local(DateTime::from_timestamp_nanos(nanos), format)
}

pub fn time_from_millis(millis: i64, format: &str) -> String {
    DateTime::from_timestamp_millis(millis).map_or_else(String::new, |time| local(time, format))
}

fn local(time: DateTime<Utc>, format: &str) -> String {
    time.with_timezone(&Local).format(format).to_string()
}

/// Returns an empty string for -1, which atuin writes when it did not record the duration.
pub fn duration(nanos: i64) -> String {
    if nanos < 0 {
        return String::new();
    }
    let ms = nanos / 1_000_000;
    match ms {
        0..1_000 => format!("{ms} ms"),
        1_000..60_000 => format!("{}.{} s", ms / 1_000, ms % 1_000 / 100),
        60_000..3_600_000 => format!("{}m {:02}s", ms / 60_000, ms / 1_000 % 60),
        _ => format!("{}h {:02}m", ms / 3_600_000, ms / 60_000 % 60),
    }
}

pub fn first_line(text: &str) -> Cow<'_, str> {
    let text = text.trim();
    match text.split_once('\n') {
        Some((first, _)) => Cow::Owned(format!("{} …", first.trim_end())),
        None => Cow::Borrowed(text),
    }
}

pub fn tilde<'a>(path: &'a str, home: Option<&str>) -> Cow<'a, str> {
    match home.and_then(|home| path.strip_prefix(home)) {
        Some("") => Cow::Borrowed("~"),
        Some(rest) if rest.starts_with('/') => Cow::Owned(format!("~{rest}")),
        _ => Cow::Borrowed(path),
    }
}

pub fn group_digits(n: impl Display) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Token counts reach the millions, so they read better as `10.7M` than in full.
pub fn compact(n: i64) -> String {
    match n {
        ..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{}.{}k", n / 1_000, n % 1_000 / 100),
        _ => format!("{}.{}M", n / 1_000_000, n % 1_000_000 / 100_000),
    }
}

pub fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

pub fn non_empty_or(text: String, fallback: &str) -> String {
    if text.is_empty() {
        fallback.to_owned()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations() {
        assert_eq!(duration(-1), "");
        assert_eq!(duration(420_000), "0 ms");
        assert_eq!(duration(999_999_999), "999 ms");
        assert_eq!(duration(59_999_000_000), "59.9 s");
        assert_eq!(duration(125_000_000_000), "2m 05s");
        assert_eq!(duration(3_720_000_000_000), "1h 02m");
    }

    #[test]
    fn shortens_home_paths() {
        let home = Some("/Users/me");
        assert_eq!(tilde("/Users/me", home), "~");
        assert_eq!(tilde("/Users/me/src", home), "~/src");
        assert_eq!(tilde("/Users/meg/src", home), "/Users/meg/src");
        assert_eq!(tilde("/tmp", None), "/tmp");
    }

    #[test]
    fn marks_multiline_text() {
        assert_eq!(first_line("ls -la"), "ls -la");
        assert_eq!(
            first_line("for x in a b\ndo echo $x\ndone"),
            "for x in a b …"
        );
        assert_eq!(first_line("\n echo hi\n"), "echo hi");
    }

    #[test]
    fn groups_digits() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(51_537), "51,537");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }

    #[test]
    fn compacts_counts() {
        assert_eq!(compact(118), "118");
        assert_eq!(compact(71_949), "71.9k");
        assert_eq!(compact(10_729_952), "10.7M");
    }

    #[test]
    fn formats_millis_and_nanos_alike() {
        let format = "%Y-%m-%d %H:%M:%S";
        assert_eq!(
            time_from_millis(1_795_158_470_099, format),
            time_from_nanos(1_795_158_470_099_000_000, format)
        );
    }
}
