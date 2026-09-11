//! OSC 9: the desktop notification the TERMINAL raises.
//!
//! The bar in `progress` shows the wait. This says it is over, to a reader who
//! is by then in another window — which is the only reader who needs telling.
//!
//! The terminal raises it, not this process. A notification library wants a
//! local desktop session, and the session that matters may be at the far end of
//! an SSH connection; the sequence travels the pipe that is already open. The
//! same argument `clipboard` makes, and it was settled there first.
//!
//! Write-only, like everything in this module. It carries a title and nothing
//! else: Ghostty's OSC 9 takes one string, and OSC 777 — the extension with a
//! separate body — it does not implement. One line, therefore, and the line has
//! to be the whole message.

use super::Wrap;

/// Preparation finished and the reviewer is open.
pub const READY: &str = "dfr: the review is ready";

/// Preparation did not finish. The reason stays on stderr, where the reader
/// finds it when they come back — a title is one line and an error is not.
pub const FAILED: &str = "dfr: preparing the review failed";

/// The bytes to write to stdout.
pub fn sequence(title: &str, wrap: Wrap) -> String {
    wrap.apply(format!("\x1b]9;{title}\x1b\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_terminal_gets_the_bare_sequence() {
        assert_eq!(
            sequence(READY, Wrap::None),
            "\x1b]9;dfr: the review is ready\x1b\\"
        );
    }

    #[test]
    fn tmux_gets_the_passthrough() {
        let s = sequence(FAILED, Wrap::Tmux);
        assert!(s.starts_with("\x1bPtmux;"), "{s:?}");
        assert!(s.ends_with("\x1b\\"), "{s:?}");
        assert!(s.contains("\x1b\x1b]9;dfr: preparing"), "{s:?}");
    }

    /// OSC 9 and OSC 9;4 share a prefix, and a terminal tells them apart by
    /// what follows the semicolon. A title opening with digits and a semicolon
    /// is therefore read as a progress command — and this crate sends real ones
    /// from `progress`, so the collision is not hypothetical.
    ///
    /// The test is on the constants because that is where the mistake would be
    /// made: by someone rewriting the copy, months from now, with no reason to
    /// know the rule exists.
    #[test]
    fn no_title_can_be_mistaken_for_a_progress_command() {
        for title in [READY, FAILED] {
            let head: String = title.chars().take_while(char::is_ascii_digit).collect();
            assert!(
                head.is_empty() || !title[head.len()..].starts_with(';'),
                "{title:?} reads as OSC 9;{head} — a progress command"
            );
        }
    }
}
