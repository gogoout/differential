//! OSC sequences: the things the terminal owns, asked for directly.
//!
//! Three of them, and one reason. The clipboard, the tab's progress bar and the
//! desktop notification all belong to the terminal emulator the reader is
//! sitting AT — not to this process, which may be a thousand miles away at the
//! other end of an SSH connection. A library that reaches for any of the three
//! needs a local desktop session; the escape sequence needs only the pipe that
//! is already there. `clipboard` explains the day that stopped being theory.
//!
//! **Every sequence here is write-only.** Nothing replies, so a terminal that
//! refuses one is indistinguishable from a terminal that acted on it. No caller
//! may treat a sent sequence as a thing that happened: say "sent", never
//! "copied", and give the reader a way through that does not depend on it.

pub mod clipboard;
pub mod notify;
pub mod progress;

use std::io::Write;

/// Which passthrough a multiplexer needs, if any.
///
/// Read once per session: the multiplexer a process is inside cannot change
/// under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    /// Straight to the terminal.
    None,
    /// `ESC Ptmux; …` with every inner ESC doubled. tmux also needs
    /// `set -g set-clipboard on` for OSC 52, which is its own default in
    /// current versions; nothing here can check it.
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

    /// `inner` as the multiplexer needs to receive it.
    ///
    /// One function for all three sequences, because the passthrough is a
    /// property of the multiplexer and not of what is being passed. It was
    /// written out once per sequence first, and the copies had already started
    /// to differ.
    pub fn apply(self, inner: String) -> String {
        match self {
            Wrap::None => inner,
            // Inside tmux the sequence is data, so its own ESC has to be
            // doubled or tmux ends the passthrough at the first one.
            Wrap::Tmux => format!("\x1bPtmux;{}\x1b\\", inner.replace('\x1b', "\x1b\x1b")),
            Wrap::Screen => format!("\x1bP{inner}\x1b\\"),
        }
    }
}

/// Put a sequence on stdout. `true` means the bytes left this process, which is
/// the most anyone here can know — see the module header.
///
/// Safe to call while ratatui holds the screen: an OSC sequence addresses the
/// terminal rather than the grid, so it moves no cursor and paints no cell. The
/// backend flushes at the end of every `draw`, so a write between draws cannot
/// land inside one.
pub fn emit(seq: &str) -> bool {
    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes())
        .and_then(|()| out.flush())
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_terminal_gets_the_sequence_untouched() {
        assert_eq!(
            Wrap::None.apply("\x1b]9;hi\x1b\\".into()),
            "\x1b]9;hi\x1b\\"
        );
    }

    #[test]
    fn tmux_needs_a_passthrough_with_its_escapes_doubled() {
        let s = Wrap::Tmux.apply("\x1b]9;hi\x1b\\".into());
        assert_eq!(s, "\x1bPtmux;\x1b\x1b]9;hi\x1b\x1b\\\x1b\\");
    }

    #[test]
    fn screen_wraps_in_a_device_control_string() {
        let s = Wrap::Screen.apply("\x1b]9;hi\x1b\\".into());
        assert_eq!(s, "\x1bP\x1b]9;hi\x1b\\\x1b\\");
    }
}
