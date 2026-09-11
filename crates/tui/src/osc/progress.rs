//! OSC 9;4: the progress bar the TERMINAL draws, on its own tab.
//!
//! The splash screen already says everything about the wait. The splash screen
//! is also the one thing a reader who started a review and switched windows is
//! not looking at, and on a grouping cache miss that wait is an agent call with
//! a twenty-minute deadline. This sequence puts the same state somewhere they
//! can see without switching back: the tab, or the taskbar entry.
//!
//! The ConEmu extension, read by Ghostty, WezTerm and Windows Terminal among
//! others. Write-only like every sequence in this module (see the module
//! header), so nothing here may report that a bar appeared.
//!
//! The terminal drops a state it has not heard from in about fifteen seconds.
//! That is a floor on how often a caller must re-send, not a reason to re-send
//! often: `Bar::set` is idempotent and the splash redraws every 120 ms, so the
//! keep-alive costs nothing and needs no clock of its own.

use super::Wrap;

/// What the bar shows.
///
/// Three of the five ConEmu states. `2` (error) and `4` (paused) are not here:
/// a failed run clears the bar and says so with a notification, because a red
/// bar left in someone's tab outlives the process that put it there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// State 1. This percentage of the run is done.
    At(u8),
    /// State 3, indeterminate. Working, and nothing here can estimate for how
    /// long — the honest answer while an agent call runs.
    Working,
    /// State 0. Nothing is running; the terminal takes the bar down.
    Clear,
}

/// The bytes to write to stdout.
pub fn sequence(state: State, wrap: Wrap) -> String {
    let inner = match state {
        // Clamped rather than asserted: a bar is not worth a panic, and a
        // terminal given 140 does something undefined rather than nothing.
        State::At(p) => format!("\x1b]9;4;1;{}\x1b\\", p.min(100)),
        State::Working => "\x1b]9;4;3\x1b\\".to_string(),
        State::Clear => "\x1b]9;4;0\x1b\\".to_string(),
    };
    wrap.apply(inner)
}

/// A bar that takes itself down.
///
/// The clear is in `Drop` because the paths out of a review are not one path:
/// the pipeline finishes, or `q` cancels it, or it errors, or it panics and
/// unwinds. Every one of those has to leave the reader's tab clean, and only
/// `Drop` covers the last two. Scope the guard to the wait, not to the process.
pub struct Bar(Wrap);

impl Bar {
    pub fn new(wrap: Wrap) -> Self {
        Bar(wrap)
    }

    pub fn set(&self, state: State) {
        super::emit(&sequence(state, self.0));
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        self.set(State::Clear);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_state_has_its_conemu_number() {
        let at = |s| sequence(s, Wrap::None);
        assert_eq!(at(State::At(37)), "\x1b]9;4;1;37\x1b\\");
        assert_eq!(at(State::Working), "\x1b]9;4;3\x1b\\");
        assert_eq!(at(State::Clear), "\x1b]9;4;0\x1b\\");
    }

    /// A percentage over 100 is a caller's arithmetic bug, and the terminal's
    /// behaviour on one is undefined. Clamping keeps the bar wrong rather than
    /// broken, which is the right way round for decoration.
    #[test]
    fn a_percentage_never_leaves_here_above_a_hundred() {
        assert_eq!(sequence(State::At(255), Wrap::None), "\x1b]9;4;1;100\x1b\\");
        assert_eq!(sequence(State::At(100), Wrap::None), "\x1b]9;4;1;100\x1b\\");
    }

    #[test]
    fn tmux_gets_the_passthrough() {
        let s = sequence(State::At(50), Wrap::Tmux);
        assert!(s.starts_with("\x1bPtmux;"), "{s:?}");
        assert!(s.ends_with("\x1b\\"), "{s:?}");
        // The inner ESC is doubled, or tmux ends the passthrough on it.
        assert!(s.contains("\x1b\x1b]9;4;1;50"), "{s:?}");
    }
}
