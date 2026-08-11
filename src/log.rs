use crate::record::now_ms;
use std::io::IsTerminal;
use std::sync::OnceLock;

static OFFSET: OnceLock<i64> = OnceLock::new();
static COLOR: OnceLock<bool> = OnceLock::new();

/// Local UTC offset in seconds, resolved once via `date +%z` so timestamps read
/// as wall-clock time without pulling in a date/time crate.
fn offset_secs() -> i64 {
    *OFFSET.get_or_init(|| {
        std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| parse_offset(s.trim()))
            .unwrap_or(0)
    })
}

fn parse_offset(s: &str) -> Option<i64> {
    if s.len() < 5 {
        return None;
    }
    let sign = match s.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hours: i64 = s.get(1..3)?.parse().ok()?;
    let mins: i64 = s.get(3..5)?.parse().ok()?;
    Some(sign * (hours * 3600 + mins * 60))
}

/// Wall-clock `HH:MM:SS` for log line prefixes.
pub fn clock() -> String {
    let secs = (now_ms() / 1000) as i64 + offset_secs();
    let day = secs.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", day / 3600, (day % 3600) / 60, day % 60)
}

fn use_color() -> bool {
    *COLOR.get_or_init(|| std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

fn paint(code: &str, s: &str) -> String {
    if use_color() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn dim(s: &str) -> String {
    paint("2", s)
}
pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn green(s: &str) -> String {
    paint("32", s)
}
pub fn yellow(s: &str) -> String {
    paint("33", s)
}
pub fn magenta(s: &str) -> String {
    paint("35", s)
}

/// Startup banner, in the spirit of a dev-server boot summary.
pub fn banner(endpoint: &str, device: &str, sources: &[&str], files: usize, ready_ms: u128) {
    let version = env!("CARGO_PKG_VERSION");
    let src = if sources.is_empty() {
        "none found".to_string()
    } else {
        sources.join(", ")
    };
    eprintln!();
    eprintln!(
        "  {} {} {}",
        magenta("▲"),
        bold("khotan-observer"),
        dim(version)
    );
    eprintln!("  {} Endpoint:   {}", dim("-"), endpoint);
    eprintln!("  {} Device:     {}", dim("-"), device);
    eprintln!("  {} Sources:    {}", dim("-"), src);
    eprintln!("  {} Tracking:   {} transcript files", dim("-"), files);
    eprintln!();
    eprintln!(
        "  {} Watching in {}",
        green("✓"),
        dim(&format!("{ready_ms}ms"))
    );
    eprintln!("  {}", dim("Ctrl-C to stop"));
    eprintln!();
}

/// One activity line: what was captured, what got delivered, what's queued.
pub fn activity(captured: usize, uploaded: usize, spool: usize, warn: Option<&str>) {
    let mut parts = Vec::new();
    if captured > 0 {
        parts.push(green(&format!("captured {captured}")));
    }
    if uploaded > 0 {
        parts.push(format!("uploaded {uploaded}"));
    }
    parts.push(dim(&format!("spool {spool}")));
    let mut line = format!("  {}  {}", dim(&clock()), parts.join("   "));
    if let Some(w) = warn {
        line.push_str(&format!("   {}", yellow(&format!("⚠ {w}"))));
    }
    eprintln!("{line}");
}

/// Periodic proof-of-life while nothing is being written.
pub fn idle(files: usize, spool: usize) {
    eprintln!(
        "  {}  {}",
        dim(&clock()),
        dim(&format!("idle — watching {files} files, spool {spool}"))
    );
}

pub fn warn(msg: &str) {
    eprintln!("  {}  {}", dim(&clock()), yellow(&format!("⚠ {msg}")));
}

#[cfg(test)]
mod tests {
    use super::parse_offset;

    #[test]
    fn parses_positive_offset() {
        assert_eq!(parse_offset("+1000"), Some(36_000));
    }

    #[test]
    fn parses_negative_offset() {
        assert_eq!(parse_offset("-0530"), Some(-19_800));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_offset("nope"), None);
        assert_eq!(parse_offset(""), None);
    }
}
