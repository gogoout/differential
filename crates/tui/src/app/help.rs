//! One table of keys, read twice: by the footer and by `?`.
//!
//! The footer used to name ten keys and the help modal named them again, in a
//! different order and a different wording — a wall the reader stops seeing,
//! and two lists to keep in step. Both now ask [`App::area`] where the reader
//! is standing and read the acts for that place, so the short list on the
//! footer is a subset of the long one in the modal by construction.
//!
//! # Adding a place
//!
//! The table is declarative on purpose: a place is a list of rows, and every
//! consumer is derived from it. Four steps, and nothing else to keep in step:
//!
//! 1. Add a variant to [`Area`], and its words to [`Area::title`].
//! 2. Say when the reader is in it, in [`App::area_of`].
//! 3. Write its rows in [`App::acts_of`] — [`Act::footer`] for the one to
//!    three a footer shows, [`Act::quiet`] for the rest.
//!
//! A row's short words go on ONE footer, never two: the window's own footer
//! for a pane, and the modal's own footer for a modal ([`App::modal_hints`]).
//! A modal names its keys in the box the reader is looking at, so the window
//! footer under it holds the pills and `? help` and nothing else.
//! 4. Add the place to `spec/tui.md` and to this crate's README.
//!
//! What a footer button presses is read from the row's own key column
//! ([`presses_for`]), so a row cannot name one key and press another.
//!
//! Built at draw time from the model, so `draw` stays a pure function of it
//! and nothing here is state.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::text::{Hint, Ink, joined};
use super::{App, Focus, Mode, ViewMode};

/// Where the reader is standing. Everything the two lists differ by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// The left pane: the reading plan, or the file tree.
    Plan,
    /// The diff pane, on an ordinary row.
    Diff,
    /// The diff pane, with a line selection open.
    Selecting,
    /// The diff pane, with the symbol float open. Not modal: `j`/`k` still
    /// move the cursor, and moving it is one of the ways out.
    Peeking,
    /// The diff pane, on a review thread the forge holds.
    Thread {
        own: bool,
        resolved: bool,
    },
    FileList,
    Findings,
    /// Writing a finding.
    Composer,
    /// A question only `y` answers: a publish, a comment's deletion.
    Question,
    /// Help itself, and a notice. Any key closes them, and that is the
    /// whole list.
    Reading,
}

impl Area {
    /// A modal takes every key it is given, so the keys that work in the
    /// review behind it do not work here — and naming them would be a lie.
    fn modal(self) -> bool {
        !matches!(
            self,
            Area::Plan | Area::Diff | Area::Selecting | Area::Peeking | Area::Thread { .. }
        )
    }

    /// What the help modal calls this place.
    fn title(self) -> &'static str {
        match self {
            Area::Plan => "the plan pane",
            Area::Diff => "the diff pane",
            Area::Selecting => "a line selection",
            Area::Peeking => "a symbol's declaration",
            Area::Thread { own: false, .. } => "a review thread",
            Area::Thread { own: true, .. } => "your own comment",
            Area::FileList => "the file list",
            Area::Findings => "the findings list",
            Area::Composer => "writing a finding",
            Area::Question => "the question",
            Area::Reading => "here",
        }
    }
}

/// One key and what it does here: as the footer says it, and as `?` does.
///
/// `footer` is the short words, and `None` keeps the row out of the footer —
/// a footer of everything is the wall this change is undoing.
pub struct Act {
    pub key: String,
    pub footer: Option<String>,
    pub help: String,
}

impl Act {
    /// A row the footer shows and the modal explains.
    fn footer(key: &str, short: &str, help: &str) -> Self {
        Act {
            key: key.to_string(),
            footer: Some(short.to_string()),
            help: help.to_string(),
        }
    }

    /// A row the modal names and the footer does not.
    fn quiet(key: &str, help: &str) -> Self {
        Act {
            key: key.to_string(),
            footer: None,
            help: help.to_string(),
        }
    }
}

/// The keys a click on this row's footer button presses, read from the key
/// column itself so a row cannot name one key and press another. The first
/// key named is the one a click presses: `j/k` is `j`, and `dd` is two `d`.
fn presses_for(key: &str) -> Vec<KeyEvent> {
    let bare = |code: KeyCode| vec![KeyEvent::new(code, KeyModifiers::NONE)];
    let first = key.split(['/', ' ']).next().unwrap_or_default();
    match first {
        "enter" => bare(KeyCode::Enter),
        "esc" => bare(KeyCode::Esc),
        "space" => bare(KeyCode::Char(' ')),
        "dd" => vec![KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE); 2],
        _ => match first.chars().next() {
            Some(c) if first.chars().count() == 1 => bare(KeyCode::Char(c)),
            // A row whose key no single press can send — the wheel, "any
            // other key". It reads, and a click on it does nothing.
            _ => Vec::new(),
        },
    }
}

/// One row as a footer button. The words are the row's, and the keys a
/// click presses are read from its key column.
fn hint(act: &Act) -> Hint {
    Hint::button(
        &format!("{} ", act.key),
        act.footer.as_deref().unwrap_or_default(),
        presses_for(&act.key),
    )
}

/// A titled run of acts in the help modal. The second one is under a rule.
pub struct HelpSection {
    pub title: &'static str,
    pub acts: Vec<Act>,
}

/// Getting about, in one place. Every one of these works from either pane,
/// so a reader hunting for "how do I move" reads one run of rows rather than
/// finding `j/k` under the place they are in and `g/G` three sections later.
///
/// `j/k` is the exception that proves it: it is the pane's, so it says what
/// it does in THIS pane. In a selection it is the selection's, and it stays
/// up in that place's own rows.
fn moving(area: Area) -> Vec<Act> {
    let mut acts = match area {
        Area::Selecting => Vec::new(),
        Area::Plan => vec![Act::quiet("j/k", "switch group")],
        _ => vec![Act::quiet("j/k", "move over rows")],
    };
    acts.extend([
        Act::quiet("J/K  { }", "previous / next group"),
        Act::quiet("n/N", "next / previous hunk"),
        Act::quiet("ctrl-d/u", "half page"),
        Act::quiet("g/G", "top / bottom"),
        Act::quiet("tab", "switch pane focus"),
    ]);
    acts
}

/// The keys that mean the same thing wherever the reader stands in the
/// review. They are why the footer can be three keys long: a key that is
/// always there does not need saying on every row.
fn everywhere() -> Vec<Act> {
    vec![
        Act::quiet("s", "unified / split diff"),
        Act::quiet("w", "soft wrap long lines"),
        Act::quiet(
            "h/l  ·  0",
            "shift the diff sideways · back to the left edge",
        ),
        Act::quiet("F", "every finding and thread, in one list"),
        Act::quiet("y", "copy the open findings"),
        Act::quiet("P", "publish the open findings (asks first)"),
        Act::quiet("R", "fetch the review threads again"),
        Act::quiet("?", "these keys"),
        Act::quiet("q  ·  ctrl-c", "quit — state is saved on every change"),
    ]
}

impl App {
    /// Where the reader is standing.
    pub fn area(&self) -> Area {
        self.area_of(&self.mode)
    }

    /// The area help is being read ABOUT: `?` was pressed somewhere, and that
    /// somewhere is what the modal answers for. The footer's own area stays
    /// `Reading` while the modal is up, which is what empties it.
    fn help_area(&self) -> Area {
        match &self.mode {
            Mode::Help(from) => self.area_of(from),
            other => self.area_of(other),
        }
    }

    fn area_of(&self, mode: &Mode) -> Area {
        match mode {
            Mode::Help(_) | Mode::Notice { .. } => Area::Reading,
            Mode::FileList { .. } => Area::FileList,
            Mode::Findings { .. } => Area::Findings,
            Mode::Editing { .. } => Area::Composer,
            Mode::Publish { .. } | Mode::DeleteComment { .. } => Area::Question,
            Mode::Normal => match self.focus {
                Focus::Groups => Area::Plan,
                // A selection is what the next key acts on, so it outranks
                // the row under the cursor.
                Focus::Detail if self.visual.is_some() => Area::Selecting,
                Focus::Detail if self.peek.is_some() => Area::Peeking,
                Focus::Detail => match self.thread_at_cursor() {
                    Some(t) => Area::Thread {
                        own: self.own_comment_at_cursor().is_some(),
                        resolved: t.resolved,
                    },
                    None => Area::Diff,
                },
            },
        }
    }

    /// Whether `?` is help's key here. In the composer it is a character,
    /// and in a `y` question every key but `y` is the answer no.
    pub(super) fn help_opens(&self) -> bool {
        match &self.mode {
            Mode::Normal | Mode::FileList { .. } => true,
            // While `D` waits for its answer, the next key IS the answer.
            Mode::Findings { confirming, .. } => !confirming,
            _ => false,
        }
    }

    /// Open help over where the reader is, keeping that place to come back to.
    pub(super) fn open_help(&mut self) {
        let from = std::mem::replace(&mut self.mode, Mode::Normal);
        self.mode = Mode::Help(Box::new(from));
    }

    /// The acts of the place the reader is standing in.
    pub fn acts(&self) -> Vec<Act> {
        self.acts_of(self.area())
    }

    /// The table. One row per key, the footer's one to three first: the
    /// footer takes them in this order and stops when the row runs out.
    fn acts_of(&self, area: Area) -> Vec<Act> {
        match area {
            // A label says what the key WILL do, not which view is already
            // on: a key named for where the reader already is reads as a key
            // that does nothing.
            Area::Plan => vec![
                Act::footer("enter", "open", "open the group or file in the diff pane"),
                Act::footer("space", "reviewed", "mark the whole group or file reviewed"),
                match self.view_mode {
                    ViewMode::Files => Act::footer("f", "plan", "back to the reading plan"),
                    ViewMode::Groups => {
                        Act::footer("f", "tree", "the file tree instead of the plan")
                    }
                },
                Act::quiet("z", "unfold a skim remainder, a noise group or a directory"),
            ],
            Area::Diff => vec![
                Act::footer("c", "note", "write a finding on this line, or on the hunk"),
                Act::footer("space", "reviewed", "mark this hunk's class reviewed"),
                Act::footer("v", "select", "start a line selection here"),
                Act::quiet("dd", "delete the finding under the cursor"),
                Act::quiet(
                    "z",
                    "on a symbol: what declares it · on a boundary: more of the \
                     file, or the hunk it names",
                ),
                Act::quiet("f", "the file list (enter jumps)"),
            ],
            Area::Peeking => vec![
                Act::footer("z", "next", "the next symbol on this line"),
                Act::footer("esc", "close", "close the float"),
                Act::quiet("j/k", "move on — the float closes with the cursor"),
            ],
            Area::Selecting => vec![
                Act::footer(
                    "j/k",
                    "extend",
                    "extend the selection, up to a context boundary",
                ),
                Act::footer("c", "note", "write one finding over the selected lines"),
                Act::footer("esc", "drop", "drop the selection"),
                Act::quiet("v", "drops it too"),
            ],
            // A thread is the forge's. What the reader may do to it depends
            // on whether the comment under the cursor is theirs.
            Area::Thread { own, resolved } => [
                vec![
                    Act::footer("r", "reply", "draft a reply under this thread"),
                    match resolved {
                        true => {
                            Act::footer("x", "reopen", "reopen the thread on the forge, at once")
                        }
                        false => {
                            Act::footer("x", "resolve", "resolve the thread on the forge, at once")
                        }
                    },
                ],
                match own {
                    true => vec![
                        Act::footer("c", "edit", "rewrite your comment on the forge"),
                        Act::footer("dd", "delete", "delete it there and here (asks first)"),
                    ],
                    false => vec![Act::quiet("c  ·  dd", "not yours — r replies to it")],
                },
                vec![Act::quiet("z", "open or close a resolved thread")],
            ]
            .into_iter()
            .flatten()
            .collect(),
            // These two draw their own footer from these same rows, so the
            // list a reader sees under the box is this list.
            Area::FileList => vec![
                Act::footer("enter", "jump", "jump to the file"),
                Act::footer("esc", "close", "close the list · so does f"),
                Act::quiet("j/k", "move over the files"),
            ],
            Area::Findings => vec![
                Act::footer("enter", "jump", "jump to the note or thread"),
                Act::footer("dd", "delete", "delete the selected note"),
                Act::footer(
                    "D",
                    "clear local",
                    "clear the notes not on the request (asks first)",
                ),
                Act::footer("y", "copy", "copy the open findings"),
                Act::footer("P", "publish", "publish the open findings (asks first)"),
                Act::footer("esc", "close", "close the list · so does F"),
                Act::quiet("j/k", "move over the list"),
            ],
            // The composer and the questions keep footers of their own: one
            // names a chord no single key sends, and the others are a
            // question rather than a list of keys.
            Area::Composer => vec![
                Act::quiet("enter  ·  ctrl-s", "save the finding"),
                Act::quiet("shift+enter", "a new line · or a trailing \\ before enter"),
                Act::quiet("esc", "discard it"),
            ],
            Area::Question => vec![
                Act::quiet("y", "yes, and only y does"),
                Act::quiet("any other key", "no — nothing happens"),
            ],
            Area::Reading => vec![Act::quiet("any key", "close this")],
        }
    }

    /// The window footer's right edge: this place's short words, then
    /// `? help`, one hint per row and no separators — how many of them fit is
    /// the footer's arithmetic, and it drops them from the left.
    ///
    /// A modal's own words are on the modal, so under one this is `? help`
    /// alone. Empty where `?` cannot be pressed — a box the caret owns takes
    /// `?` as a character, and a `y` question takes it as its no.
    pub fn footer_hints(&self) -> Vec<Hint> {
        if !self.help_opens() {
            return Vec::new();
        }
        let own = match self.area().modal() {
            true => Vec::new(),
            false => self.modal_hints(),
        };
        own.into_iter()
            .chain([hint(&Act::footer("?", "help", "these keys"))])
            .collect()
    }

    /// A modal's own footer, exactly as drawn: this place's short words with
    /// a separator between them. The hit test reads the same list, so a
    /// click lands on the button the reader sees.
    pub fn modal_footer(&self) -> Vec<Hint> {
        // A lead of two columns, so the first key does not sit against the
        // border. It presses nothing: it is padding, and a click on padding
        // is a click on nothing.
        let mut hints = vec![Hint::note("  ", Ink::Dim)];
        hints.extend(joined(&self.modal_hints()));
        hints
    }

    /// The short words of this place, as buttons: what a modal draws on its
    /// own footer, and what the window footer draws when no modal is up.
    pub fn modal_hints(&self) -> Vec<Hint> {
        self.acts()
            .iter()
            .filter(|a| a.footer.is_some())
            .map(hint)
            .collect()
    }

    /// What `?` shows: this place's keys, and — outside a modal — the keys
    /// that work anywhere in the review, under a rule.
    pub fn help_sections(&self) -> Vec<HelpSection> {
        let area = self.help_area();
        let mut sections = vec![HelpSection {
            title: area.title(),
            acts: self.acts_of(area),
        }];
        if !area.modal() {
            // Acting, then moving: the reader who opens `?` is usually asking
            // what they can DO here, and the movement keys are the ones they
            // already know. Last is where a reference belongs.
            sections.push(HelpSection {
                title: "anywhere",
                acts: everywhere(),
            });
            sections.push(HelpSection {
                title: "moving",
                acts: moving(area),
            });
        }
        sections
    }
}
