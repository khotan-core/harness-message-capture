use crate::record::now_ms;
use owo_colors::{OwoColorize, Stream::Stderr};
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

/// The Khotan logomark: the three brand shapes drawn on the character grid,
/// carrying the same extruded edge as the wordmark. Every row is `MARK_COLS`
/// characters wide, padded so the wordmark beside it stays in one column.
const MARK: [&str; 7] = [
    " ███████╗        ",
    " ██████╔╝ █████╗ ",
    " ████╔═╝████████║",
    " ════╝  ████████║",
    " ██████══███████║",
    "████████║═██████║",
    "════════╝ ══════╝",
];

const MARK_COLS: usize = 17;

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
    // Some non-interactive shells export `COLUMNS=0`. Treat that as unknown
    // rather than as a window too narrow to draw anything.
    std::env::var("COLUMNS")
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|c| *c > 0)
}

/// The wordmark is for a person watching `khotan-observer run`. Block art in
/// the LaunchAgent log would only bloat a file nobody reads for decoration.
fn wordmark_fits(is_tty: bool, cols: Option<usize>) -> bool {
    is_tty && cols.is_none_or(|c| c >= WORDMARK_MIN_COLS)
}

/// The mark joins the wordmark only when the window is wide enough to hold
/// both on one line.
fn lockup_fits(is_tty: bool, cols: Option<usize>) -> bool {
    is_tty && cols.is_none_or(|c| c >= LOCKUP_MIN_COLS)
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
    if lockup_fits(is_tty, cols) {
        // The mark stands taller than the wordmark. Lead with the extra mark
        // rows so both shadow baselines finish on the same line. A row with no
        // lettering beside it is trimmed rather than padded out with blanks.
        let lead = MARK.len() - WORDMARK.len();
        for (i, mark) in MARK.iter().enumerate() {
            match i.checked_sub(lead).and_then(|w| WORDMARK.get(w)) {
                Some(word) => eprintln!("  {}  {}", coral(mark), coral(word)),
                None => eprintln!("  {}", coral(mark.trim_end())),
            }
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

/// One workspace's counts and reason for a single scan pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    pub label: String,
    pub captured: usize,
    pub uploaded: usize,
    pub skipped: usize,
    pub queued: usize,
    pub tone: Tone,
    pub means: Option<String>,
}

impl Activity {
    pub fn new(label: impl Into<String>) -> Activity {
        Activity {
            label: label.into(),
            captured: 0,
            uploaded: 0,
            skipped: 0,
            queued: 0,
            tone: Tone::Delivery,
            means: None,
        }
    }

    pub fn set_means(&mut self, tone: Tone, means: impl Into<String>) {
        if tone_rank(tone) >= tone_rank(self.tone) {
            self.tone = tone;
            self.means = Some(means.into());
        }
    }

    pub fn is_empty(&self) -> bool {
        self.captured == 0
            && self.uploaded == 0
            && self.skipped == 0
            && self.queued == 0
            && self.means.is_none()
    }

    fn is_skip_only(&self) -> bool {
        self.captured == 0 && self.uploaded == 0 && self.queued == 0 && self.skipped > 0
    }

    fn render(&self) -> String {
        let mut parts = vec![paint(self.tone, &self.label)];
        if self.captured > 0 {
            parts.push(format!(
                "{} {}",
                "captured".if_supports_color(Stderr, |t| t.dimmed()),
                self.captured,
            ));
        }
        if self.uploaded > 0 {
            parts.push(format!(
                "{} {}",
                "uploaded".if_supports_color(Stderr, |t| t.dimmed()),
                green(&self.uploaded.to_string()),
            ));
        }
        if self.skipped > 0 {
            parts.push(format!(
                "{} {}",
                "skipped".if_supports_color(Stderr, |t| t.dimmed()),
                orange(&self.skipped.to_string()),
            ));
        }
        if self.queued > 0 {
            parts.push(format!(
                "{} {}",
                "queued".if_supports_color(Stderr, |t| t.dimmed()),
                orange(&self.queued.to_string()),
            ));
        }
        if let Some(means) = &self.means {
            parts.push(paint(self.tone, &format!("({means})")));
        }
        parts.join("   ")
    }
}

fn tone_rank(tone: Tone) -> u8 {
    match tone {
        Tone::Delivery => 0,
        Tone::Warning => 1,
        Tone::Error => 2,
    }
}

/// How many skip-only repos print in full before they fold into `+N more`.
pub const MAX_SKIP_LINES: usize = 8;

/// Keep every capture, upload, queue, or error line. Fold extra skip-only
/// lines so a first-time catch-up does not flood the log.
pub fn fold_skips(lines: Vec<Activity>, max_skips: usize) -> Vec<Activity> {
    let (mut keep, mut skip_only): (Vec<_>, Vec<_>) =
        lines.into_iter().partition(|line| !line.is_skip_only());
    keep.sort_by(|left, right| left.label.cmp(&right.label));
    skip_only.sort_by(|left, right| {
        right
            .skipped
            .cmp(&left.skipped)
            .then_with(|| left.label.cmp(&right.label))
    });
    if skip_only.len() > max_skips {
        let rest = skip_only.split_off(max_skips);
        let extra = rest.len();
        let mut more = Activity::new(format!("+{extra} more"));
        more.skipped = rest.iter().map(|line| line.skipped).sum();
        more.queued = rest.iter().map(|line| line.queued).sum();
        more.tone = Tone::Warning;
        skip_only.push(more);
    }
    keep.extend(skip_only);
    keep
}

/// One line per workspace, sharing one timestamp for the pass.
pub fn activities(lines: &[Activity]) {
    let stamp = dim(&clock());
    for line in lines {
        if line.is_empty() {
            continue;
        }
        eprintln!("  {stamp}   {}", line.render());
    }
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

fn alert_body(msg: &str) -> String {
    format!(" ALERT  {msg} ")
}

/// Bright-red reverse bar. Used only for "a newer observer is out" so that
/// line cannot sit in the same orange as a skip warning.
pub fn alert(msg: &str) {
    let painted = alert_body(msg)
        .if_supports_color(Stderr, |t| {
            format!("{}", t.on_truecolor(255, 32, 32).bright_white().bold())
        })
        .to_string();
    eprintln!();
    eprintln!("  {}   {}", dim(&clock()), painted);
    eprintln!();
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
    fn mark_rows_share_one_width() {
        for line in super::MARK {
            assert_eq!(line.chars().count(), super::MARK_COLS, "{line}");
        }
    }

    /// The last row of each block is its shadow baseline. Leading with the
    /// mark's extra rows lands both baselines on the same output line.
    #[test]
    fn mark_and_wordmark_share_a_baseline() {
        assert!(super::MARK.len() >= super::WORDMARK.len());
        let lead = super::MARK.len() - super::WORDMARK.len();
        for i in lead..super::MARK.len() {
            assert!(super::WORDMARK.get(i - lead).is_some());
        }
        assert!(super::MARK[super::MARK.len() - 1].ends_with('╝'));
        assert!(super::WORDMARK[super::WORDMARK.len() - 1].ends_with('╝'));
    }

    #[test]
    fn lockup_needs_width() {
        assert!(super::lockup_fits(true, None));
        assert!(!super::lockup_fits(false, None));
        assert!(super::lockup_fits(true, Some(super::LOCKUP_MIN_COLS)));
        assert!(!super::lockup_fits(true, Some(super::LOCKUP_MIN_COLS - 1)));
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
    fn alert_body_leads_with_alert() {
        assert_eq!(
            super::alert_body("Newer observer v0.1.20 is out (this binary is 0.1.19)"),
            " ALERT  Newer observer v0.1.20 is out (this binary is 0.1.19) "
        );
    }

    #[test]
    fn attributed_puts_means_in_parens() {
        assert_eq!(
            super::attributed("podium-automation", "Send worked, local delete failed"),
            "podium-automation (Send worked, local delete failed)"
        );
    }

    fn skip_line(label: &str, skipped: usize) -> super::Activity {
        let mut line = super::Activity::new(label);
        line.skipped = skipped;
        line.tone = super::Tone::Warning;
        line.means = Some("Repo is real, but not on the allow list".into());
        line
    }

    #[test]
    fn render_puts_counts_on_the_repo_line() {
        let mut line = skip_line("usi", 432);
        line.queued = 200;
        assert_eq!(
            line.render(),
            "usi   skipped 432   queued 200   (Repo is real, but not on the allow list)"
        );
    }

    #[test]
    fn render_keeps_a_delivery_failure_on_its_own_line() {
        let mut line = super::Activity::new("dev-serve-robotics");
        line.queued = 188;
        line.set_means(super::Tone::Warning, "Host is up in DNS, port is closed");
        assert_eq!(
            line.render(),
            "dev-serve-robotics   queued 188   (Host is up in DNS, port is closed)"
        );
    }

    #[test]
    fn fold_skips_keeps_queue_lines_and_caps_skip_only() {
        let mut queued = super::Activity::new("dev-serve-robotics");
        queued.queued = 188;
        queued.set_means(super::Tone::Warning, "Host is up in DNS, port is closed");
        let lines = vec![
            skip_line("usi", 432),
            skip_line("brain", 10),
            skip_line("entry", 20),
            queued,
        ];
        let folded = super::fold_skips(lines, 2);
        assert_eq!(folded[0].label, "dev-serve-robotics");
        assert_eq!(folded[0].queued, 188);
        assert_eq!(folded[1].label, "usi");
        assert_eq!(folded[2].label, "entry");
        assert_eq!(folded[3].label, "+1 more");
        assert_eq!(folded[3].skipped, 10);
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
