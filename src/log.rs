use crate::record::now_ms;
use owo_colors::{OwoColorize, Stream::Stderr};
use std::sync::OnceLock;

static OFFSET: OnceLock<i64> = OnceLock::new();

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

/// Group digits so large file counts stay readable (e.g. `3,950`).
#[allow(clippy::manual_is_multiple_of)]
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn green(s: &str) -> String {
    s.if_supports_color(Stderr, |t| t.green()).to_string()
}

pub fn dim(s: &str) -> String {
    s.if_supports_color(Stderr, |t| t.dimmed()).to_string()
}

/// A `label   value` line in the startup summary. The label is padded before
/// styling so ANSI codes never throw the column alignment off.
fn row(label: &str, value: &str) {
    let padded = format!("{label:<9}");
    eprintln!(
        "    {}  {}",
        padded.if_supports_color(Stderr, |t| t.dimmed()),
        value
    );
}

/// Startup summary printed once when the watcher comes up.
pub fn banner(device: &str, sources: &[&str], files: usize, routes: usize, ready_ms: u128) {
    let version = env!("CARGO_PKG_VERSION");
    let src = if sources.is_empty() {
        "none found".to_string()
    } else {
        sources.join(", ")
    };
    eprintln!();
    eprintln!(
        "  {}  {}",
        "khotan-observer".if_supports_color(Stderr, |t| t.bold()),
        version.if_supports_color(Stderr, |t| t.dimmed()),
    );
    eprintln!();
    row("Device", device);
    row("Sources", &src);
    row("Routes", &format!("{} customer destination(s)", routes));
    row(
        "Tracking",
        &format!("{} transcript files", thousands(files)),
    );
    eprintln!();
    eprintln!(
        "  {} Watching in {}  {}",
        "✓".if_supports_color(Stderr, |t| t.green()),
        format!("{ready_ms}ms").if_supports_color(Stderr, |t| t.dimmed()),
        "· Ctrl-C to stop".if_supports_color(Stderr, |t| t.dimmed()),
    );
    eprintln!();
}

/// One activity line: what was captured, what got delivered, and any delivery
/// backlog. A healthy empty queue is intentionally omitted.
/// `threads` is an optional workspace/chat label summary (e.g. `harness-message-capture`).
pub fn activity(
    captured: usize,
    uploaded: usize,
    skipped: usize,
    spool: usize,
    threads: Option<&str>,
    warn: Option<&str>,
) {
    let mut parts: Vec<String> = Vec::new();
    if captured > 0 {
        parts.push(format!(
            "{} {}",
            "captured".if_supports_color(Stderr, |t| t.dimmed()),
            captured.if_supports_color(Stderr, |t| t.green()),
        ));
    }
    if uploaded > 0 {
        parts.push(format!(
            "{} {}",
            "uploaded".if_supports_color(Stderr, |t| t.dimmed()),
            uploaded.if_supports_color(Stderr, |t| t.cyan()),
        ));
    }
    if skipped > 0 {
        parts.push(format!(
            "{} {}",
            "skipped".if_supports_color(Stderr, |t| t.dimmed()),
            skipped.if_supports_color(Stderr, |t| t.yellow()),
        ));
    }
    // A backlog only matters when delivery could not complete; don't clutter
    // healthy capture lines with an implementation detail.
    if spool > 0 {
        parts.push(format!(
            "{} {}",
            "queued".if_supports_color(Stderr, |t| t.dimmed()),
            spool.if_supports_color(Stderr, |t| t.yellow()),
        ));
    }
    if let Some(t) = threads.filter(|s| !s.is_empty()) {
        parts.push(t.if_supports_color(Stderr, |s| s.magenta()).to_string());
    }

    let mut line = format!("  {}   {}", dim(&clock()), parts.join("   "));
    if let Some(w) = warn {
        line.push_str(&format!(
            "   {}",
            format!("⚠ {w}").if_supports_color(Stderr, |t| t.yellow())
        ));
    }
    eprintln!("{line}");
}

/// Periodic proof-of-life while nothing is being written.
pub fn idle(files: usize, _spool: usize) {
    eprintln!(
        "  {}   {}",
        dim(&clock()),
        dim(&format!("idle · watching {} files", thousands(files),)),
    );
}

pub fn warn(msg: &str) {
    eprintln!(
        "  {}   {}",
        dim(&clock()),
        format!("⚠ {msg}").if_supports_color(Stderr, |t| t.yellow())
    );
}

#[cfg(test)]
mod tests {
    use super::{parse_offset, thousands};

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

    #[test]
    fn groups_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(950), "950");
        assert_eq!(thousands(3_950), "3,950");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
