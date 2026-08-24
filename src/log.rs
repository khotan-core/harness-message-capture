use crate::record::now_ms;
use owo_colors::{OwoColorize, Stream::Stderr, Style};
use std::io::IsTerminal;
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

pub fn orange(s: &str) -> String {
    s.if_supports_color(Stderr, |t| t.truecolor(234, 138, 45))
        .to_string()
}

pub fn red(s: &str) -> String {
    s.if_supports_color(Stderr, |t| t.red()).to_string()
}

/// Brand coral, sampled from the Khotan logo. Reserved for the wordmark so the
/// tone stays distinct from `orange`, which means a warning.
pub fn coral(s: &str) -> String {
    s.if_supports_color(Stderr, |t| t.truecolor(240, 74, 34))
        .to_string()
}

pub fn dim(s: &str) -> String {
    s.if_supports_color(Stderr, |t| t.dimmed()).to_string()
}

/// `podium-automation (Send worked, local delete failed)`
pub fn attributed(label: &str, means: &str) -> String {
    format!("{label} ({means})")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Delivery,
    Warning,
    Error,
}

fn paint(tone: Tone, s: &str) -> String {
    match tone {
        Tone::Delivery => green(s),
        Tone::Warning => orange(s),
        Tone::Error => red(s),
    }
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

fn stop_hint() -> &'static str {
    stop_hint_for(std::env::var_os(crate::agent::BACKGROUND_MODE_ENV).as_deref())
}

fn stop_hint_for(mode: Option<&std::ffi::OsStr>) -> &'static str {
    if mode == Some(std::ffi::OsStr::new("background")) {
        "· khotan-observer stop to quit"
    } else {
        "· Ctrl-C to stop"
    }
}

fn allow_line(allow: &[String], routes: usize) -> String {
    if allow.is_empty() {
        return "none".to_string();
    }
    let names = allow.join(", ");
    let ready = if routes == 1 {
        "1 ready".to_string()
    } else {
        format!("{routes} ready")
    };
    format!("{names} · {ready}")
}

/// The Khotan logomark: indigo shapes on a coral tile. One character in this
/// grid is one pixel, `#` indigo and `.` coral. Two pixel rows print as one
/// text row, so the mark keeps a square aspect next to the wordmark.
const MARK: [&str; 12] = [
    "..............",
    ".#######......",
    ".#######..#...",
    ".######..####.",
    ".#####..#####.",
    ".####..######.",
    ".......######.",
    ".#####.######.",
    ".######.#####.",
    ".#######.####.",
    ".#######.####.",
    "..............",
];

const MARK_COLS: usize = 14;

const INDIGO: (u8, u8, u8) = (79, 95, 146);
const CORAL: (u8, u8, u8) = (240, 74, 34);

/// One text row of the mark. `▀` paints the upper pixel as foreground and the
/// lower pixel as background, so a single cell carries both.
fn mark_row(row: usize) -> String {
    let upper = MARK[row * 2].as_bytes();
    let lower = MARK[row * 2 + 1].as_bytes();
    let mut out = String::with_capacity(MARK_COLS * 24);
    for col in 0..MARK_COLS {
        let (fr, fg, fb) = if upper[col] == b'#' { INDIGO } else { CORAL };
        let (br, bg, bb) = if lower[col] == b'#' { INDIGO } else { CORAL };
        let style = Style::new().truecolor(fr, fg, fb).on_truecolor(br, bg, bb);
        out.push_str(
            &"▀"
                .if_supports_color(Stderr, |t| t.style(style))
                .to_string(),
        );
    }
    out
}

/// The mark is only legible in colour; without it every cell collapses to an
/// identical `▀`. Probe the same machinery the rest of the output uses.
fn colour_enabled() -> bool {
    "x".if_supports_color(Stderr, |t| t.red()).to_string() != "x"
}

/// The Khotan wordmark, drawn directly on the character grid so every stroke
/// is two cells wide. Every row is `WORDMARK_COLS` characters wide.
const WORDMARK: [&str; 6] = [
    "██╗  ██╗██╗  ██╗ ██████╗ ████████╗ █████╗ ███╗   ██╗",
    "██║ ██╔╝██║  ██║██╔═══██╗╚══██╔══╝██╔══██╗████╗  ██║",
    "█████╔╝ ███████║██║   ██║   ██║   ███████║██╔██╗ ██║",
    "██╔═██╗ ██╔══██║██║   ██║   ██║   ██╔══██║██║╚██╗██║",
    "██║  ██╗██║  ██║╚██████╔╝   ██║   ██║  ██║██║ ╚████║",
    "╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝    ╚═╝   ╚═╝  ╚═╝╚═╝  ╚═══╝",
];

const WORDMARK_COLS: usize = 52;

/// Art plus the two-space indent on each side.
const WORDMARK_MIN_COLS: usize = WORDMARK_COLS + 4;

/// The full lockup: mark, a two-space gap, wordmark, and the outer indent.
const LOCKUP_MIN_COLS: usize = MARK_COLS + 2 + WORDMARK_MIN_COLS;

/// Terminal width, when we can learn it without a syscall crate. `COLUMNS` is
/// the only source we trust here; `None` means "unknown, assume wide enough".
fn term_cols() -> Option<usize> {
    std::env::var("COLUMNS").ok()?.trim().parse().ok()
}

/// The wordmark is for a person watching `khotan-observer run`. Block art in
/// the LaunchAgent log would only bloat a file nobody reads for decoration.
fn wordmark_fits(is_tty: bool, cols: Option<usize>) -> bool {
    is_tty && cols.is_none_or(|c| c >= WORDMARK_MIN_COLS)
}

/// The mark joins the wordmark only when it is legible and the window is wide
/// enough to hold both on one line.
fn lockup_fits(is_tty: bool, cols: Option<usize>, colour: bool) -> bool {
    is_tty && colour && cols.is_none_or(|c| c >= LOCKUP_MIN_COLS)
}

/// Startup summary printed once when the watcher comes up.
pub fn banner(sources: &[&str], routes: usize, allow: &[String], ready_ms: u128) {
    let version = env!("CARGO_PKG_VERSION");
    let src = if sources.is_empty() {
        "none found".to_string()
    } else {
        sources.join(", ")
    };
    let is_tty = std::io::stderr().is_terminal();
    let cols = term_cols();
    eprintln!();
    if lockup_fits(is_tty, cols, colour_enabled()) {
        for (i, line) in WORDMARK.iter().enumerate() {
            eprintln!("  {}  {}", mark_row(i), coral(line));
        }
        eprintln!();
        eprintln!(
            "  {}  {}",
            "observer".if_supports_color(Stderr, |t| t.bold()),
            version.if_supports_color(Stderr, |t| t.dimmed()),
        );
    } else if wordmark_fits(is_tty, cols) {
        for line in WORDMARK {
            eprintln!("  {}", coral(line));
        }
        eprintln!();
        eprintln!(
            "  {}  {}",
            "observer".if_supports_color(Stderr, |t| t.bold()),
            version.if_supports_color(Stderr, |t| t.dimmed()),
        );
    } else {
        eprintln!(
            "  {}  {}",
            "khotan-observer".if_supports_color(Stderr, |t| t.bold()),
            version.if_supports_color(Stderr, |t| t.dimmed()),
        );
    }
    eprintln!();
    row("Sources", &src);
    row("Allow", &allow_line(allow, routes));
    eprintln!();
    eprintln!(
        "  {} Watching in {}  {}",
        "✓".if_supports_color(Stderr, |t| t.green()),
        format!("{ready_ms}ms").if_supports_color(Stderr, |t| t.dimmed()),
        stop_hint().if_supports_color(Stderr, |t| t.dimmed()),
    );
    eprintln!();
}

/// One activity line: what was captured, what got delivered, and any delivery
/// backlog. A healthy empty queue is intentionally omitted.
/// `threads` is an optional workspace/chat label summary (e.g. `harness-message-capture (cursor)`).
pub fn activity(
    captured: usize,
    uploaded: usize,
    skipped: usize,
    spool: usize,
    notes: &[(Tone, String)],
) {
    let mut parts: Vec<String> = Vec::new();
    if captured > 0 {
        parts.push(format!(
            "{} {}",
            "captured".if_supports_color(Stderr, |t| t.dimmed()),
            captured,
        ));
    }
    if uploaded > 0 {
        parts.push(format!(
            "{} {}",
            "uploaded".if_supports_color(Stderr, |t| t.dimmed()),
            green(&uploaded.to_string()),
        ));
    }
    if skipped > 0 {
        parts.push(format!(
            "{} {}",
            "skipped".if_supports_color(Stderr, |t| t.dimmed()),
            orange(&skipped.to_string()),
        ));
    }
    // A backlog only matters when delivery could not complete; don't clutter
    // healthy capture lines with an implementation detail.
    if spool > 0 {
        parts.push(format!(
            "{} {}",
            "queued".if_supports_color(Stderr, |t| t.dimmed()),
            orange(&spool.to_string()),
        ));
    }
    for (tone, note) in notes {
        parts.push(paint(*tone, note));
    }

    eprintln!("  {}   {}", dim(&clock()), parts.join("   "));
}

/// Periodic proof-of-life while nothing is being written.
pub fn idle(files: usize, _spool: usize) {
    eprintln!(
        "  {}   {}",
        dim(&clock()),
        dim(&format!(
            "idle (No new lines this pass · {} files)",
            thousands(files)
        )),
    );
}

pub fn warn(msg: &str) {
    eprintln!("  {}   {}", dim(&clock()), orange(msg));
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

    #[test]
    fn wordmark_rows_share_one_width() {
        for line in super::WORDMARK {
            assert_eq!(line.chars().count(), super::WORDMARK_COLS, "{line}");
        }
    }

    #[test]
    fn mark_is_a_rectangle_of_two_pixel_values() {
        assert_eq!(super::MARK.len() % 2, 0, "needs whole text rows");
        for line in super::MARK {
            assert_eq!(line.chars().count(), super::MARK_COLS, "{line}");
            assert!(line.chars().all(|c| c == '#' || c == '.'), "{line}");
        }
    }

    #[test]
    fn mark_row_emits_one_cell_per_column() {
        for row in 0..super::MARK.len() / 2 {
            let rendered = super::mark_row(row);
            assert_eq!(
                rendered.chars().filter(|c| *c == '▀').count(),
                super::MARK_COLS
            );
        }
    }

    #[test]
    fn lockup_needs_colour_and_width() {
        assert!(super::lockup_fits(true, None, true));
        assert!(!super::lockup_fits(true, None, false));
        assert!(!super::lockup_fits(false, None, true));
        assert!(super::lockup_fits(true, Some(super::LOCKUP_MIN_COLS), true));
        assert!(!super::lockup_fits(
            true,
            Some(super::LOCKUP_MIN_COLS - 1),
            true
        ));
    }

    #[test]
    fn wordmark_stays_out_of_the_log_file() {
        assert!(!super::wordmark_fits(false, None));
        assert!(!super::wordmark_fits(false, Some(200)));
    }

    #[test]
    fn wordmark_needs_room_to_avoid_wrapping() {
        assert!(super::wordmark_fits(true, None));
        assert!(super::wordmark_fits(true, Some(super::WORDMARK_MIN_COLS)));
        assert!(!super::wordmark_fits(
            true,
            Some(super::WORDMARK_MIN_COLS - 1)
        ));
    }

    #[test]
    fn attributed_puts_means_in_parens() {
        assert_eq!(
            super::attributed("podium-automation", "Send worked, local delete failed"),
            "podium-automation (Send worked, local delete failed)"
        );
    }

    #[test]
    fn allow_line_joins_names_and_ready_count() {
        assert_eq!(super::allow_line(&[], 0), "none");
        assert_eq!(
            super::allow_line(&["podium-automation".into(), "chief".into()], 2),
            "podium-automation, chief · 2 ready"
        );
        assert_eq!(
            super::allow_line(&["podium-automation".into()], 1),
            "podium-automation · 1 ready"
        );
    }

    #[test]
    fn foreground_hint_uses_ctrl_c() {
        assert_eq!(super::stop_hint_for(None), "· Ctrl-C to stop");
    }

    #[test]
    fn background_hint_uses_stop_command() {
        assert_eq!(
            super::stop_hint_for(Some(std::ffi::OsStr::new("background"))),
            "· khotan-observer stop to quit"
        );
    }
}
