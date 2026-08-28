//! OSC 52: putting text on the clipboard of the terminal the reader is AT.
//!
//! `arboard` needs a local display server, which a remote session does not
//! have — so over SSH `y` reported `clipboard unavailable` and the summary was
//! unreachable from inside the reviewer. The clipboard the reader wants is on
//! their own machine, and no library running on the remote host can reach it.
//!
//! The escape sequence can. The remote host does not interpret it: the local
//! terminal emulator does, and that terminal owns the real clipboard.
//!
//! **It is write-only.** There is no reply to read, so a terminal that refuses
//! the sequence — several do, for good reasons — fails silently and looks
//! exactly like one that took it. That is why the caller must never treat a
//! sent sequence as a copy that landed: the way out is the command the footer
//! names, `dfr findings <range> --summary`, which prints the same text.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// What a terminal will carry. Several cap OSC 52 at about 8 KB, and the
/// payload that matters is the BASE64, not the text.
///
/// Deliberately the small cap even outside tmux, which allows just under 75 KB:
/// the sequence is unacknowledged, so a payload over a terminal's own limit is
/// dropped or truncated with nothing to say so. Refusing to send is the only
/// honest answer, and the file the caller writes is the way out.
const MAX_PAYLOAD: usize = 8192;

/// Which passthrough a multiplexer needs, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    /// Straight to the terminal.
    None,
    /// `ESC Ptmux; …` with every inner ESC doubled. tmux also needs
    /// `set -g set-clipboard on`, which is its own default in current
    /// versions; nothing here can check it.
    Tmux,
    /// GNU screen's device-control string.
    Screen,
}

impl Wrap {
    /// Read the environment once. `$TMUX` is set inside tmux even when `$TERM`
    /// says something else, so it is checked first.
    pub fn detect() -> Self {
        if std::env::var_os("TMUX").is_some() {
            return Wrap::Tmux;
        }
        match std::env::var("TERM") {
            Ok(t) if t.starts_with("screen") => Wrap::Screen,
            _ => Wrap::None,
        }
    }
}

/// The bytes to write to stdout, or `None` when the payload is over the cap.
pub fn sequence(text: &str, wrap: Wrap) -> Option<String> {
    let payload = STANDARD.encode(text);
    if payload.len() > MAX_PAYLOAD {
        return None;
    }
    // `c` is the system clipboard, as opposed to the primary selection.
    let inner = format!("\x1b]52;c;{payload}\x07");
    Some(match wrap {
        Wrap::None => inner,
        // Inside tmux the sequence is data, so its own ESC has to be doubled
        // or tmux ends the passthrough at the first one.
        Wrap::Tmux => format!("\x1bPtmux;{}\x1b\\", inner.replace('\x1b', "\x1b\x1b")),
        Wrap::Screen => format!("\x1bP{inner}\x1b\\"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_terminal_gets_the_bare_sequence() {
        let s = sequence("hi", Wrap::None).unwrap();
        assert_eq!(s, "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn tmux_needs_a_passthrough_with_its_escapes_doubled() {
        let s = sequence("hi", Wrap::Tmux).unwrap();
        assert!(s.starts_with("\x1bPtmux;"));
        assert!(s.ends_with("\x1b\\"));
        // The inner ESC is doubled, or tmux ends the passthrough on it.
        assert!(s.contains("\x1b\x1b]52;c;aGk="));
    }

    #[test]
    fn screen_wraps_in_a_device_control_string() {
        let s = sequence("hi", Wrap::Screen).unwrap();
        assert_eq!(s, "\x1bP\x1b]52;c;aGk=\x07\x1b\\");
    }

    #[test]
    fn an_oversized_payload_is_refused_rather_than_cut() {
        // Base64 is 4 bytes per 3, so this crosses the cap while the text
        // itself does not.
        let big = "x".repeat(MAX_PAYLOAD);
        assert!(sequence(&big, Wrap::None).is_none());
        let ok = "x".repeat(MAX_PAYLOAD / 2);
        assert!(sequence(&ok, Wrap::None).is_some());
    }
}
