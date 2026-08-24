//! Interactive checkbox list for `khotan-observer configure`.
//!
//! Raw terminal mode comes from libc, which the dependency tree already pulls
//! in, so the picker costs the always-on binary no new TUI crate.

use anyhow::{bail, Result};
use owo_colors::{OwoColorize, Stream::Stderr};
use std::io::{IsTerminal, Read, Write};

/// Rows shown at once. A longer list scrolls inside this window.
const WINDOW: usize = 12;

/// One selectable repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// Exact string written to `allow_repos` when this row stays checked.
    pub entry: String,
    /// Folder name in the first column.
    pub label: String,
    /// Path, or why the row cannot upload.
    pub detail: String,
    pub selected: bool,
    /// A destination file this broken cannot upload, so the row is shown for
    /// the reason alone and space does nothing.
    pub disabled: bool,
}

/// True when a person can see and answer the picker. A LaunchAgent, an
/// installer pipe, and CI all fail this and must use `--allow-repo`.
pub fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Draw the list and block until the person saves or cancels. `Ok(None)` means
/// they cancelled and the caller must leave the config alone.
pub fn run(rows: Vec<Choice>) -> Result<Option<Vec<String>>> {
    if rows.is_empty() {
        return Ok(None);
    }
    let mut state = State::new(rows);
    let _raw = RawMode::enable()?;
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 32];
    let mut drawn = 0usize;

    loop {
        drawn = state.draw(drawn)?;
        let read = stdin.read(&mut buf)?;
        if read == 0 {
            state.erase(drawn)?;
            return Ok(None);
        }
        match state.apply(&buf[..read]) {
            Outcome::Continue => {}
            Outcome::Save => {
                state.erase(drawn)?;
                return Ok(Some(state.entries()));
            }
            Outcome::Cancel => {
                state.erase(drawn)?;
                return Ok(None);
            }
        }
    }
}

enum Outcome {
    Continue,
    Save,
    Cancel,
}

struct State {
    rows: Vec<Choice>,
    filter: String,
    cursor: usize,
    offset: usize,
}

impl State {
    fn new(rows: Vec<Choice>) -> State {
        State {
            rows,
            filter: String::new(),
            cursor: 0,
            offset: 0,
        }
    }

    fn visible(&self) -> Vec<usize> {
        visible_indices(&self.rows, &self.filter)
    }

    fn entries(&self) -> Vec<String> {
        selected_entries(&self.rows)
    }

    fn apply(&mut self, bytes: &[u8]) -> Outcome {
        for key in parse_keys(bytes) {
            match key {
                Key::Up => self.step(-1),
                Key::Down => self.step(1),
                Key::Toggle => self.toggle(),
                Key::Confirm => return Outcome::Save,
                Key::Cancel => return Outcome::Cancel,
                // Escape backs out of the filter first so one stray keystroke
                // never throws away the whole selection.
                Key::Escape => {
                    if self.filter.is_empty() {
                        return Outcome::Cancel;
                    }
                    self.filter.clear();
                    self.clamp();
                }
                Key::Backspace => {
                    self.filter.pop();
                    self.clamp();
                }
                Key::Char(c) => {
                    self.filter.push(c);
                    self.cursor = 0;
                    self.clamp();
                }
            }
        }
        Outcome::Continue
    }

    fn step(&mut self, delta: isize) {
        let count = self.visible().len();
        if count == 0 {
            return;
        }
        let last = count - 1;
        self.cursor = match delta {
            d if d < 0 && self.cursor == 0 => last,
            d if d < 0 => self.cursor - 1,
            _ if self.cursor >= last => 0,
            _ => self.cursor + 1,
        };
        self.clamp();
    }

    fn toggle(&mut self) {
        let visible = self.visible();
        let Some(&index) = visible.get(self.cursor) else {
            return;
        };
        if self.rows[index].disabled {
            return;
        }
        self.rows[index].selected = !self.rows[index].selected;
    }

    /// Keep the cursor on a real row and the scroll window around it.
    fn clamp(&mut self) {
        let count = self.visible().len();
        self.cursor = self.cursor.min(count.saturating_sub(1));
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        if self.cursor >= self.offset + WINDOW {
            self.offset = self.cursor + 1 - WINDOW;
        }
        let max_offset = count.saturating_sub(WINDOW);
        self.offset = self.offset.min(max_offset);
    }

    fn erase(&self, drawn: usize) -> Result<()> {
        let mut out = std::io::stderr();
        if drawn > 0 {
            write!(out, "\r\x1b[{drawn}A\x1b[0J")?;
        }
        out.flush()?;
        Ok(())
    }

    /// Redraw in place over the previous frame. Returns the new line count.
    fn draw(&self, drawn: usize) -> Result<usize> {
        let visible = self.visible();
        let mut out = std::io::stderr();
        let mut frame = String::new();
        if drawn > 0 {
            frame.push_str(&format!("\r\x1b[{drawn}A\x1b[0J"));
        }

        let checked = self.rows.iter().filter(|row| row.selected).count();
        let found = self.rows.len();
        let files = if found == 1 { "file" } else { "files" };
        frame.push_str(&line(&format!(
            "  {}  {}",
            "Repositories to observe".if_supports_color(Stderr, |t| t.bold()),
            dim(&format!("{checked} selected")),
        )));
        frame.push_str(&line(&format!(
            "  {}",
            dim(&format!("Detected {found} env.khotan.local {files}"))
        )));
        frame.push_str(&line(&format!(
            "  {}",
            dim("↑↓ move · space toggle · type to filter · enter save · esc cancel")
        )));
        frame.push_str(&line(""));

        let width = visible
            .iter()
            .map(|&i| self.rows[i].label.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(12, 32);

        if visible.is_empty() {
            frame.push_str(&line(&format!("  {}", dim("no repository matches"))));
        }
        for (position, &index) in visible.iter().enumerate().skip(self.offset).take(WINDOW) {
            let row = &self.rows[index];
            let here = position == self.cursor;
            let pointer = if here { "›" } else { " " };
            let label = pad(&row.label, width);
            let label = match (row.disabled, here) {
                (true, _) => dim(&label),
                (false, true) => label.if_supports_color(Stderr, |t| t.bold()).to_string(),
                (false, false) => label,
            };
            let box_ = match (row.disabled, row.selected) {
                (true, _) => dim("[-]"),
                (false, true) => "[x]".if_supports_color(Stderr, |t| t.green()).to_string(),
                (false, false) => dim("[ ]"),
            };
            frame.push_str(&line(&format!(
                "  {} {} {}  {}",
                pointer.if_supports_color(Stderr, |t| t.green()),
                box_,
                label,
                dim(&row.detail)
            )));
        }

        let hidden = visible.len().saturating_sub(self.offset + WINDOW);
        if hidden > 0 {
            frame.push_str(&line(&format!("  {}", dim(&format!("… {hidden} more")))));
        }
        frame.push_str(&line(""));
        frame.push_str(&line(&format!(
            "  {} {}",
            dim("filter:"),
            if self.filter.is_empty() {
                dim("(type to narrow)")
            } else {
                self.filter.clone()
            }
        )));

        let lines = frame.matches('\n').count();
        write!(out, "{frame}")?;
        out.flush()?;
        Ok(lines)
    }
}

/// Raw mode needs an explicit carriage return on every line.
fn line(body: &str) -> String {
    format!("{body}\r\n")
}

fn dim(s: &str) -> String {
    s.if_supports_color(Stderr, |t| t.dimmed()).to_string()
}

fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - len))
}

/// Rows whose label contains the filter, matched without case.
pub fn visible_indices(rows: &[Choice], filter: &str) -> Vec<usize> {
    let needle = filter.trim().to_ascii_lowercase();
    rows.iter()
        .enumerate()
        .filter(|(_, row)| needle.is_empty() || row.label.to_ascii_lowercase().contains(&needle))
        .map(|(index, _)| index)
        .collect()
}

/// Entries to write, in the order the list showed them.
pub fn selected_entries(rows: &[Choice]) -> Vec<String> {
    rows.iter()
        .filter(|row| row.selected)
        .map(|row| row.entry.clone())
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Toggle,
    Confirm,
    Cancel,
    Escape,
    Backspace,
    Char(char),
}

/// Terminals deliver an arrow key as one three-byte write, so parsing a whole
/// read buffer avoids a second blocking read to disambiguate escape.
fn parse_keys(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            0x1b => {
                let next = bytes.get(i + 1).copied();
                let third = bytes.get(i + 2).copied();
                // `[` is the normal cursor mode, `O` the application mode some
                // terminals switch to.
                if matches!(next, Some(b'[') | Some(b'O')) {
                    match third {
                        Some(b'A') => keys.push(Key::Up),
                        Some(b'B') => keys.push(Key::Down),
                        _ => {}
                    }
                    i += 3;
                } else {
                    keys.push(Key::Escape);
                    i += 1;
                }
            }
            0x03 | 0x04 => {
                keys.push(Key::Cancel);
                i += 1;
            }
            0x0e => {
                keys.push(Key::Down);
                i += 1;
            }
            0x10 => {
                keys.push(Key::Up);
                i += 1;
            }
            b'\r' | b'\n' => {
                keys.push(Key::Confirm);
                i += 1;
            }
            0x08 | 0x7f => {
                keys.push(Key::Backspace);
                i += 1;
            }
            b' ' => {
                keys.push(Key::Toggle);
                i += 1;
            }
            byte if byte.is_ascii_graphic() => {
                keys.push(Key::Char(byte as char));
                i += 1;
            }
            _ => i += 1,
        }
    }
    keys
}

/// Restores the terminal when the picker returns. `panic = "abort"` skips this
/// on a panic, so the draw loop keeps no fallible work beyond terminal writes.
struct RawMode {
    saved: libc::termios,
}

impl RawMode {
    fn enable() -> Result<RawMode> {
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut saved) != 0 {
                bail!("could not read terminal settings");
            }
            let mut raw = saved;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                bail!("could not switch the terminal to raw mode");
            }
            // Hide the cursor so it does not trail the redraw.
            let mut err = std::io::stderr();
            let _ = write!(err, "\x1b[?25l");
            let _ = err.flush();
            Ok(RawMode { saved })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved);
        }
        let mut err = std::io::stderr();
        let _ = write!(err, "\x1b[?25h");
        let _ = err.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(label: &str, selected: bool) -> Choice {
        Choice {
            entry: label.to_string(),
            label: label.to_string(),
            detail: String::new(),
            selected,
            disabled: false,
        }
    }

    fn rows() -> Vec<Choice> {
        vec![
            choice("podium-automation", true),
            choice("chief-nutrition", false),
            choice("khotan-core", false),
        ]
    }

    #[test]
    fn filter_matches_any_position_without_case() {
        let rows = rows();
        assert_eq!(visible_indices(&rows, ""), vec![0, 1, 2]);
        assert_eq!(visible_indices(&rows, "NUTRI"), vec![1]);
        assert_eq!(visible_indices(&rows, "o"), vec![0, 1, 2]);
        assert!(visible_indices(&rows, "zzz").is_empty());
    }

    #[test]
    fn only_checked_rows_are_written() {
        let mut rows = rows();
        assert_eq!(selected_entries(&rows), vec!["podium-automation"]);
        rows[2].selected = true;
        assert_eq!(
            selected_entries(&rows),
            vec!["podium-automation", "khotan-core"]
        );
        for row in rows.iter_mut() {
            row.selected = false;
        }
        assert!(selected_entries(&rows).is_empty());
    }

    #[test]
    fn space_toggles_only_the_row_under_the_cursor() {
        let mut state = State::new(rows());
        state.apply(b" ");
        assert!(!state.rows[0].selected);
        state.apply(&[0x1b, b'[', b'B']);
        state.apply(b" ");
        assert!(state.rows[1].selected);
        assert_eq!(state.entries(), vec!["chief-nutrition"]);
    }

    #[test]
    fn typing_narrows_the_list_and_toggles_the_match() {
        let mut state = State::new(rows());
        state.apply(b"nutri");
        assert_eq!(state.visible(), vec![1]);
        state.apply(b" ");
        assert_eq!(
            state.entries(),
            vec!["podium-automation", "chief-nutrition"]
        );
    }

    #[test]
    fn backspace_and_escape_walk_the_filter_back() {
        let mut state = State::new(rows());
        state.apply(b"nutri");
        state.apply(&[0x7f]);
        assert_eq!(state.filter, "nutr");
        state.apply(&[0x1b]);
        assert_eq!(state.filter, "");
        assert_eq!(state.visible(), vec![0, 1, 2]);
    }

    #[test]
    fn escape_on_an_empty_filter_cancels() {
        let mut state = State::new(rows());
        assert!(matches!(state.apply(&[0x1b]), Outcome::Cancel));
    }

    #[test]
    fn enter_saves_and_ctrl_c_cancels() {
        let mut state = State::new(rows());
        assert!(matches!(state.apply(b"\r"), Outcome::Save));
        assert!(matches!(state.apply(&[0x03]), Outcome::Cancel));
    }

    #[test]
    fn cursor_wraps_at_both_ends() {
        let mut state = State::new(rows());
        state.apply(&[0x1b, b'[', b'A']);
        assert_eq!(state.cursor, 2);
        state.apply(&[0x1b, b'[', b'B']);
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn arrow_keys_never_reach_the_filter() {
        assert_eq!(parse_keys(&[0x1b, b'[', b'A']), vec![Key::Up]);
        assert_eq!(parse_keys(&[0x1b, b'O', b'B']), vec![Key::Down]);
        let mut state = State::new(rows());
        state.apply(&[0x1b, b'[', b'A']);
        assert_eq!(state.filter, "");
    }

    #[test]
    fn a_filter_with_no_match_leaves_the_selection_alone() {
        let mut state = State::new(rows());
        state.apply(b"zzz");
        state.apply(b" ");
        state.apply(&[0x1b, b'[', b'B']);
        assert_eq!(state.entries(), vec!["podium-automation"]);
    }

    #[test]
    fn an_empty_list_never_opens() {
        assert_eq!(run(Vec::new()).unwrap(), None);
    }

    #[test]
    fn space_cannot_tick_a_repo_that_would_never_upload() {
        let mut rows = rows();
        rows[0].selected = false;
        rows[0].disabled = true;
        let mut state = State::new(rows);
        state.apply(b" ");
        assert!(!state.rows[0].selected);
        assert!(state.entries().is_empty());
    }

    #[test]
    fn a_disabled_row_still_filters_and_does_not_block_the_rows_below() {
        let mut rows = rows();
        rows[0].disabled = true;
        rows[0].selected = false;
        let mut state = State::new(rows);
        assert_eq!(state.visible(), vec![0, 1, 2]);
        state.apply(&[0x1b, b'[', b'B']);
        state.apply(b" ");
        assert_eq!(state.entries(), vec!["chief-nutrition"]);
    }
}
