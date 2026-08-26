//! Which lines of a file the diff pane shows around its hunks — pure
//! arithmetic over the ranges the engine already recorded.
//!
//! The reviewer used to diff and syntax-highlight whole blobs and then search
//! the result for each hunk. It never needed to: a canonical `-U0` hunk states
//! exactly which lines changed on both sides, and everything between two hunks
//! is unchanged by construction. So the pane's content is a handful of line
//! ranges, computed here and read straight out of the blobs (ADR 0021).
//!
//! Deliberately free of `Repo`, `Style` and `Frame`: the interesting decisions
//! — how far a window reaches, when two windows merge, how many lines stay
//! hidden — are arithmetic, and arithmetic is worth testing without a terminal
//! or a repository.
//!
//! **A window stops at the neighbouring hunk until told to cross it.** Between
//! two hunks the old/new line offset is constant, which is what lets a context
//! stretch carry both sides' numbers from one length; across a hunk it is not.
//! So a crossed hunk is emitted as a `Change` segment, where both sides carry
//! their own numbers explicitly, rather than being flattened into context —
//! and a reviewer asks for it by name, never receives it by accident.

use std::collections::HashMap;
use std::ops::Range;

use differential_engine::schema;

/// How far one hunk has been pulled open past the defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Expansion {
    /// Extra context lines in the CURRENT outermost gap, on top of `context`.
    pub up: usize,
    pub down: usize,
    /// Neighbouring hunks absorbed in that direction.
    ///
    /// Crossing one resets the gap counter above it, because the gap that
    /// counter measured is no longer the outermost one. A hunk is absorbed
    /// whole or not at all and never costs context budget: showing half a
    /// change would be worse than showing none.
    pub crossed_up: usize,
    pub crossed_down: usize,
}

/// Which end of a block a boundary row sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Up,
    Down,
}

/// A boundary row: press `z` here and `hunk` grows on `side`.
///
/// Two states, and the row is drawn in both. `hidden > 0` means context lines
/// remain in the current gap. `hidden == 0` with `next: Some(..)` means the
/// gap is exhausted and the thing beyond it is another hunk — the row then
/// NAMES it, so crossing is always a deliberate second press rather than
/// something a long expansion does silently.
///
/// Both zero and `next: None` is a real file edge, and the only case where no
/// boundary row is drawn at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary {
    /// The shown hunk whose expansion state this edge grows.
    pub hunk: usize,
    pub side: Side,
    /// Context lines still hidden in the current gap.
    ///
    /// Where two blocks meet, BOTH boundaries carry the same figure: they are
    /// two ends of one gap, and a reviewer opening one end watches the other's
    /// count fall too.
    pub hidden: usize,
    /// How many lines the gap holds in total, which is what identifies two
    /// boundaries as ends of the same one.
    pub gap: usize,
    /// The hunk immediately past the gap, once the gap is exhausted.
    pub next: Option<usize>,
}

/// One stretch of the file to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Unchanged lines, present on both sides with the same content.
    Context {
        old_from: usize,
        new_from: usize,
        len: usize,
    },
    /// One hunk's changed lines. Either range is empty for a pure insertion or
    /// deletion.
    Change {
        hunk: usize,
        /// This view does not list the hunk — it was pulled in by expanding
        /// across it, and is rendered as belonging to another group.
        foreign: bool,
        old: Range<usize>,
        new: Range<usize>,
    },
}

/// A contiguous run of the file: one or more hunks whose windows have met,
/// with the context around them and a boundary row at each end that still has
/// something behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub top: Option<Boundary>,
    pub segments: Vec<Segment>,
    pub bottom: Option<Boundary>,
}

/// One hunk of a file, as the planner needs it: its canonical index, whether
/// this view lists it for reading, and the entry itself.
pub struct Candidate<'a> {
    pub index: usize,
    pub shown: bool,
    pub entry: &'a schema::HunkEntry,
}

/// Where one side of a hunk cuts the file.
///
/// `above_end` is the exclusive end of the unchanged run before the change;
/// `below_start` the first unchanged line after it. For a side with no lines
/// at all (`count == 0` — the other side inserted or deleted) both collapse to
/// the same point, so that side contributes no changed lines and its context
/// runs straight through.
#[derive(Debug, Clone)]
struct Cut {
    above_end: usize,
    below_start: usize,
    changed: Range<usize>,
}

fn cut(start: u32, count: u32) -> Cut {
    let (start, count) = (start as usize, count as usize);
    if count == 0 {
        Cut {
            above_end: start + 1,
            below_start: start + 1,
            changed: start..start,
        }
    } else {
        Cut {
            above_end: start,
            below_start: start + count,
            changed: start..start + count,
        }
    }
}

/// Plan the blocks for one file.
///
/// `hunks` must be every hunk of the file in position order — including the
/// ones this view does not list, because they are what bounds a window.
/// `context` is the default either side; `expansion` adds to it per hunk.
///
/// `Expansion::crossed_up`/`crossed_down` count HUNKS to absorb, and they
/// **saturate**: asking to cross further than there are hunks yields everything
/// to that end of the file rather than panicking or wrapping. A reviewer only
/// reaches a count by pressing `z` on a boundary that named the next hunk, so
/// in practice they stay in range — but that is the caller's habit, not a
/// precondition this function imposes, and it is total either way.
pub fn plan(
    hunks: &[Candidate],
    expansion: &HashMap<usize, Expansion>,
    context: usize,
    old_len: usize,
    new_len: usize,
) -> Vec<Block> {
    let cuts: Vec<(Cut, Cut)> = hunks
        .iter()
        .map(|c| {
            (
                cut(c.entry.old_start, c.entry.old_count),
                cut(c.entry.new_start, c.entry.new_count),
            )
        })
        .collect();

    // Lines available either side of each hunk before the neighbouring hunk
    // (or the file edge). Both sides must have them, so the smaller wins — in
    // a well-formed document the two agree.
    let avail_up = |i: usize| -> usize {
        let (old, new) = (&cuts[i].0, &cuts[i].1);
        let (prev_old, prev_new) = match i.checked_sub(1) {
            Some(p) => (cuts[p].0.below_start, cuts[p].1.below_start),
            None => (1, 1),
        };
        old.above_end
            .saturating_sub(prev_old)
            .min(new.above_end.saturating_sub(prev_new))
    };
    let avail_down = |i: usize| -> usize {
        let (old, new) = (&cuts[i].0, &cuts[i].1);
        let (next_old, next_new) = match cuts.get(i + 1) {
            Some((o, n)) => (o.above_end, n.above_end),
            None => (old_len + 1, new_len + 1),
        };
        next_old
            .saturating_sub(old.below_start)
            .min(next_new.saturating_sub(new.below_start))
    };
    let want = |i: usize| -> (usize, usize) {
        let e = expansion.get(&hunks[i].index).copied().unwrap_or_default();
        (context + e.up, context + e.down)
    };

    // Which hunks each shown hunk's block covers: itself, plus the ones it has
    // been told to cross. Absorbing is by hunk, so the span is an index range.
    let span = |i: usize| -> (usize, usize) {
        let e = expansion.get(&hunks[i].index).copied().unwrap_or_default();
        (
            i.saturating_sub(e.crossed_up),
            (i + e.crossed_down).min(hunks.len() - 1),
        )
    };
    // Context reach beyond a span's ends, clamped to the gap that is there.
    let reach_up = |i: usize, lo: usize| want(i).0.min(avail_up(lo));
    let reach_down = |i: usize, hi: usize| want(i).1.min(avail_down(hi));

    // Merge the spans. Two join when their hunk ranges overlap, or when they
    // are adjacent and their context reaches cover the whole gap between.
    struct Span {
        lo: usize,
        hi: usize,
        /// The shown hunks whose expansion governs each end — what `z` grows.
        up_owner: usize,
        down_owner: usize,
    }
    let mut spans: Vec<Span> = Vec::new();
    for i in (0..hunks.len()).filter(|&i| hunks[i].shown) {
        let (lo, hi) = span(i);
        let joins = spans.last().is_some_and(|s| {
            lo <= s.hi
                || (lo == s.hi + 1
                    && reach_down(s.down_owner, s.hi) + reach_up(i, lo) >= avail_down(s.hi))
        });
        match spans.last_mut() {
            Some(s) if joins => {
                // BOTH ends. Widening only the lower one would drop the hunks
                // between `lo` and `s.lo` from the block while still claiming
                // to have absorbed them — a hunk asked for and never drawn.
                if lo < s.lo {
                    s.lo = lo;
                    s.up_owner = i;
                }
                if hi > s.hi {
                    s.hi = hi;
                    s.down_owner = i;
                }
            }
            _ => spans.push(Span {
                lo,
                hi,
                up_owner: i,
                down_owner: i,
            }),
        }
    }

    let mut blocks: Vec<Block> = spans
        .into_iter()
        .map(|s| {
            let (up_avail, down_avail) = (avail_up(s.lo), avail_down(s.hi));
            let up = reach_up(s.up_owner, s.lo);
            let down = reach_down(s.down_owner, s.hi);

            let mut segments = Vec::new();
            if up > 0 {
                let (old, new) = (&cuts[s.lo].0, &cuts[s.lo].1);
                segments.push(Segment::Context {
                    old_from: old.above_end - up,
                    new_from: new.above_end - up,
                    len: up,
                });
            }
            for i in s.lo..=s.hi {
                if i > s.lo {
                    // The gap to the previous hunk. Fully covered, either
                    // because the two windows met or because crossing takes
                    // everything up to the hunk it absorbed.
                    let (prev_old, prev_new) = (&cuts[i - 1].0, &cuts[i - 1].1);
                    let len = avail_down(i - 1);
                    if len > 0 {
                        segments.push(Segment::Context {
                            old_from: prev_old.below_start,
                            new_from: prev_new.below_start,
                            len,
                        });
                    }
                }
                let (old, new) = (&cuts[i].0, &cuts[i].1);
                segments.push(Segment::Change {
                    hunk: hunks[i].index,
                    foreign: !hunks[i].shown,
                    old: old.changed.clone(),
                    new: new.changed.clone(),
                });
            }
            if down > 0 {
                let (old, new) = (&cuts[s.hi].0, &cuts[s.hi].1);
                segments.push(Segment::Context {
                    old_from: old.below_start,
                    new_from: new.below_start,
                    len: down,
                });
            }

            // An edge with lines left offers them; an edge with none offers the
            // hunk beyond, if there is one. Nothing at all only at a file edge.
            let edge =
                |hidden: usize, gap: usize, side: Side, owner: usize, beyond: Option<usize>| {
                    let next = (hidden == 0).then_some(beyond).flatten();
                    (hidden > 0 || next.is_some()).then(|| Boundary {
                        hunk: hunks[owner].index,
                        side,
                        hidden,
                        gap,
                        next,
                    })
                };
            Block {
                top: edge(
                    up_avail - up,
                    up_avail,
                    Side::Up,
                    s.up_owner,
                    s.lo.checked_sub(1).map(|p| hunks[p].index),
                ),
                segments,
                bottom: edge(
                    down_avail - down,
                    down_avail,
                    Side::Down,
                    s.down_owner,
                    hunks.get(s.hi + 1).map(|c| c.index),
                ),
            }
        })
        .collect();

    // Two blocks that meet describe ONE gap, and both boundary rows should
    // report what is left of it. Each was reporting only its own side's
    // remainder, so opening the top one by ten left the bottom one still
    // claiming the old number for lines it could no longer reach.
    for i in 1..blocks.len() {
        let (above, below) = blocks.split_at_mut(i);
        let (Some(a), Some(b)) = (above[i - 1].bottom.as_mut(), below[0].top.as_mut()) else {
            continue;
        };
        // Same gap only if they are adjacent — a hunk between them is two gaps.
        if a.next.is_some() || b.next.is_some() || a.gap != b.gap {
            continue;
        }
        let shared = a.hidden + b.hidden - a.gap.min(a.hidden + b.hidden);
        a.hidden = shared;
        b.hidden = shared;
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hunk entry with only the fields the planner reads.
    fn entry(old_start: u32, old_count: u32, new_start: u32, new_count: u32) -> schema::HunkEntry {
        schema::HunkEntry {
            id: format!("h{new_start}"),
            file: "f".into(),
            old_start,
            old_count,
            new_start,
            new_count,
            class: "C0".into(),
            digest: String::new(),
            nonl_old: false,
            nonl_new: false,
            forge_position: schema::ForgePosition {
                new_line: None,
                old_line: None,
            },
        }
    }

    fn shown(index: usize, e: &schema::HunkEntry) -> Candidate<'_> {
        Candidate {
            index,
            shown: true,
            entry: e,
        }
    }

    /// Context expansion only — the common case in these tests.
    fn grow(up: usize, down: usize) -> Expansion {
        Expansion {
            up,
            down,
            ..Expansion::default()
        }
    }

    /// A change segment for a hunk this view lists.
    fn own(hunk: usize, old: Range<usize>, new: Range<usize>) -> Segment {
        Segment::Change {
            hunk,
            foreign: false,
            old,
            new,
        }
    }

    fn ctx(seg: &Segment) -> (usize, usize, usize) {
        match seg {
            Segment::Context {
                old_from,
                new_from,
                len,
            } => (*old_from, *new_from, *len),
            other => panic!("expected context, got {other:?}"),
        }
    }

    #[test]
    fn a_modification_gets_context_either_side_from_both_blobs() {
        // Lines 20..22 replaced by 20..23: the old side resumes at 23, the new
        // at 24, and the context above shares a number.
        let e = entry(20, 3, 20, 4);
        let blocks = plan(&[shown(7, &e)], &HashMap::new(), 3, 100, 101);
        assert_eq!(blocks.len(), 1);
        let b = &blocks[0];
        assert_eq!(ctx(&b.segments[0]), (17, 17, 3));
        assert_eq!(b.segments[1], own(7, 20..23, 20..24));
        assert_eq!(ctx(&b.segments[2]), (23, 24, 3));
        // 16 lines above, 78 below, still hidden.
        assert_eq!(b.top.as_ref().unwrap().hidden, 16);
        assert_eq!(b.bottom.as_ref().unwrap().hidden, 100 - 23 + 1 - 3);
    }

    /// `@@ -5,0 +6,3 @@`: nothing removed, so the old side contributes no
    /// changed lines and its context runs straight through the insertion point.
    #[test]
    fn a_pure_insertion_has_no_old_side_lines() {
        let e = entry(5, 0, 6, 3);
        let blocks = plan(&[shown(0, &e)], &HashMap::new(), 2, 50, 53);
        let b = &blocks[0];
        assert_eq!(ctx(&b.segments[0]), (4, 4, 2));
        assert_eq!(b.segments[1], own(0, 5..5, 6..9));
        assert_eq!(ctx(&b.segments[2]), (6, 9, 2));
    }

    /// `@@ -6,3 +5,0 @@`: the mirror image.
    #[test]
    fn a_pure_deletion_has_no_new_side_lines() {
        let e = entry(6, 3, 5, 0);
        let blocks = plan(&[shown(0, &e)], &HashMap::new(), 2, 53, 50);
        let b = &blocks[0];
        assert_eq!(ctx(&b.segments[0]), (4, 4, 2));
        assert_eq!(b.segments[1], own(0, 6..9, 5..5));
        assert_eq!(ctx(&b.segments[2]), (9, 6, 2));
    }

    #[test]
    fn the_boundary_disappears_at_a_file_edge() {
        // A hunk at line 2 of a 10-line file: two lines above, and the default
        // context of three already covers them.
        let e = entry(2, 1, 2, 1);
        let blocks = plan(&[shown(0, &e)], &HashMap::new(), 3, 10, 10);
        let b = &blocks[0];
        assert!(b.top.is_none(), "nothing above line 2 stays hidden");
        assert_eq!(ctx(&b.segments[0]), (1, 1, 1));
        assert!(b.bottom.is_some());

        // Expanding past the end of the file clamps rather than overruns.
        let far = HashMap::from([(0, grow(0, 999))]);
        let b = &plan(&[shown(0, &e)], &far, 3, 10, 10)[0];
        assert!(b.bottom.is_none());
        assert_eq!(ctx(b.segments.last().unwrap()), (3, 3, 8));
    }

    #[test]
    fn expansion_grows_one_side_only() {
        let e = entry(50, 2, 50, 2);
        let up = HashMap::from([(4, grow(10, 0))]);
        let b = &plan(&[shown(4, &e)], &up, 3, 200, 200)[0];
        assert_eq!(ctx(&b.segments[0]), (37, 37, 13));
        assert_eq!(ctx(&b.segments[2]), (52, 52, 3), "down is untouched");
        assert_eq!(b.top.as_ref().unwrap().hidden, 49 - 13);
    }

    #[test]
    fn windows_that_meet_merge_into_one_block() {
        // 20..21 and 30..31: nine unchanged lines between them (22..30).
        let a = entry(20, 2, 20, 2);
        let z = entry(30, 2, 30, 2);
        let hunks = [shown(0, &a), shown(1, &z)];

        // Default context of three leaves a gap, so two blocks.
        let blocks = plan(&hunks, &HashMap::new(), 3, 100, 100);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].bottom.is_some() && blocks[1].top.is_some());

        // Pull the first one down far enough to close the gap.
        let merged = HashMap::from([(0, grow(0, 8))]);
        let blocks = plan(&hunks, &merged, 3, 100, 100);
        assert_eq!(blocks.len(), 1, "the windows touch, so the block is one");
        let b = &blocks[0];
        // No boundary between them, and the joining context is exactly the gap.
        assert_eq!(
            b.segments,
            vec![
                Segment::Context {
                    old_from: 17,
                    new_from: 17,
                    len: 3
                },
                own(0, 20..22, 20..22),
                Segment::Context {
                    old_from: 22,
                    new_from: 22,
                    len: 8
                },
                own(1, 30..32, 30..32),
                Segment::Context {
                    old_from: 32,
                    new_from: 32,
                    len: 3
                },
            ]
        );
        // The surviving boundaries name the block's outer hunks.
        assert_eq!(b.top.as_ref().unwrap().hunk, 0);
        assert_eq!(b.bottom.as_ref().unwrap().hunk, 1);
    }

    /// `@@ -0,0 +1 @@` — an insertion before the first line. `old_start` is
    /// zero here, which is the one place the 1-based arithmetic could underflow.
    #[test]
    fn an_insertion_at_the_very_top_of_a_file_has_nothing_above_it() {
        let e = entry(0, 0, 1, 1);
        let blocks = plan(&[shown(0, &e)], &HashMap::new(), 3, 4, 5);
        assert_eq!(blocks.len(), 1);
        let b = &blocks[0];
        assert!(b.top.is_none(), "there is no line above line 1");
        assert_eq!(b.segments[0], own(0, 0..0, 1..2));
        // Below, the old side resumes at line 1 while the new side is at 2.
        assert_eq!(ctx(&b.segments[1]), (1, 2, 3));

        // And asking for a hundred lines up cannot conjure any.
        let huge = HashMap::from([(0, grow(100, 0))]);
        assert!(plan(&[shown(0, &e)], &huge, 3, 4, 5)[0].top.is_none());
    }

    /// A deletion of the whole file: nothing on the new side at all.
    #[test]
    fn a_file_emptied_completely_still_plans() {
        let e = entry(1, 3, 0, 0);
        let b = &plan(&[shown(0, &e)], &HashMap::new(), 3, 3, 0)[0];
        assert!(b.top.is_none() && b.bottom.is_none());
        assert_eq!(b.segments, vec![own(0, 1..4, 0..0)]);
    }

    /// A window stops at its neighbour, and SAYS so: the boundary survives
    /// with nothing left to show and the hunk beyond it named. Silently
    /// vanishing here was indistinguishable from the end of the file.
    #[test]
    fn a_window_stops_at_an_unlisted_hunk_until_told_to_cross() {
        let a = entry(20, 2, 20, 2);
        let hidden = entry(26, 1, 26, 4);
        let z = entry(40, 2, 43, 2);
        let hunks = [
            shown(0, &a),
            Candidate {
                index: 1,
                shown: false,
                entry: &hidden,
            },
            shown(2, &z),
        ];
        // Even asking for a hundred lines each way, neither window reaches past
        // the unlisted hunk on its own, and the two never merge.
        let huge = HashMap::from([(0, grow(100, 100)), (2, grow(100, 100))]);
        let blocks = plan(&hunks, &huge, 3, 200, 203);
        assert_eq!(blocks.len(), 2);
        // Below the first hunk: lines 22..25 only — four, up to `above_end` 26.
        assert_eq!(ctx(blocks[0].segments.last().unwrap()), (22, 22, 4));
        assert!(
            blocks[0]
                .segments
                .iter()
                .all(|s| !matches!(s, Segment::Change { hunk: 1, .. })),
            "an unlisted hunk must not render until it is asked for"
        );
        // The wall is visible: nothing left in the gap, and the hunk named.
        assert_eq!(
            blocks[0].bottom,
            Some(Boundary {
                hunk: 0,
                side: Side::Down,
                hidden: 0,
                gap: 4,
                next: Some(1),
            })
        );
        // And from the other side, the same wall.
        assert_eq!(ctx(&blocks[1].segments[0]), (27, 30, 13));
        assert_eq!(blocks[1].top.as_ref().unwrap().next, Some(1));
    }

    #[test]
    fn crossing_absorbs_the_hunk_whole_and_opens_the_gap_beyond() {
        let a = entry(20, 2, 20, 2);
        let hidden = entry(26, 1, 26, 4);
        let hunks = [
            shown(0, &a),
            Candidate {
                index: 1,
                shown: false,
                entry: &hidden,
            },
        ];
        let crossed = HashMap::from([(
            0,
            Expansion {
                crossed_down: 1,
                ..Expansion::default()
            },
        )]);
        let blocks = plan(&hunks, &crossed, 3, 200, 203);
        assert_eq!(blocks.len(), 1, "the crossed hunk joins the block");
        let b = &blocks[0];
        assert_eq!(
            b.segments,
            vec![
                Segment::Context {
                    old_from: 17,
                    new_from: 17,
                    len: 3
                },
                own(0, 20..22, 20..22),
                // The whole gap comes with it — crossing takes everything up to
                // the hunk it absorbed.
                Segment::Context {
                    old_from: 22,
                    new_from: 22,
                    len: 4
                },
                // Marked foreign: real code, but not on this reading list.
                Segment::Change {
                    hunk: 1,
                    foreign: true,
                    old: 26..27,
                    new: 26..30
                },
                Segment::Context {
                    old_from: 27,
                    new_from: 30,
                    len: 3
                },
            ]
        );
        // A fresh gap beyond it, counted from the crossed hunk's far edge.
        let bottom = b.bottom.as_ref().unwrap();
        assert_eq!(bottom.hunk, 0, "z still grows the hunk the reviewer owns");
        assert_eq!(bottom.hidden, 200 - 27 + 1 - 3);
        assert_eq!(bottom.next, None, "nothing beyond but the file's end");
    }

    /// A hunk is absorbed whole and costs no context budget — showing half a
    /// change would be worse than showing none.
    /// A merge has to widen BOTH ends of the span.
    ///
    /// Widening only the lower one drops every hunk between the new `lo` and
    /// the old — absorbed as far as the arithmetic is concerned, never drawn.
    /// The UI cannot reach this today (merging leaves the upper end owned by
    /// the earlier hunk, so it is that hunk's `crossed_up` a `z` grows), but
    /// `plan` is a pure function and should not have a silently-wrong corner
    /// waiting for its caller to change.
    #[test]
    fn a_merge_widens_both_ends_of_the_span() {
        let x = entry(10, 1, 10, 1);
        let a = entry(20, 1, 20, 1);
        let c = entry(30, 1, 30, 1);
        let hunks = [
            Candidate {
                index: 0,
                shown: false,
                entry: &x,
            },
            shown(1, &a),
            shown(2, &c),
        ];
        // The LATER shown hunk reaches back past the earlier one's span.
        let deep = HashMap::from([(
            2,
            Expansion {
                crossed_up: 2,
                ..Expansion::default()
            },
        )]);
        let blocks = plan(&hunks, &deep, 3, 100, 100);
        assert_eq!(blocks.len(), 1);
        let drawn: Vec<usize> = blocks[0]
            .segments
            .iter()
            .filter_map(|s| match s {
                Segment::Change { hunk, .. } => Some(*hunk),
                _ => None,
            })
            .collect();
        assert_eq!(
            drawn,
            vec![0, 1, 2],
            "every hunk the span covers must be drawn"
        );
        // And the top boundary belongs to the hunk that reached up there.
        assert_eq!(blocks[0].top.as_ref().map(|b| b.hunk), Some(2));
    }

    /// Two boundaries over one gap are two ends of the same thing, so both
    /// report what is left of it. Each reporting only its own side meant
    /// opening the top by ten left the bottom still claiming the old figure for
    /// lines it could no longer reach.
    #[test]
    fn both_ends_of_one_gap_report_the_same_remainder() {
        let a = entry(20, 2, 20, 2);
        let z = entry(60, 2, 60, 2);
        let hunks = [shown(0, &a), shown(1, &z)];

        let idle = plan(&hunks, &HashMap::new(), 3, 200, 200);
        assert_eq!(idle.len(), 2, "the gap is far too wide to have merged");
        let (top, bottom) = (
            idle[0].bottom.as_ref().unwrap().hidden,
            idle[1].top.as_ref().unwrap().hidden,
        );
        assert_eq!(top, bottom, "an untouched gap already agreed");

        // Open it from the TOP only; both ends must fall by the same ten.
        let opened = HashMap::from([(0, grow(0, 10))]);
        let after = plan(&hunks, &opened, 3, 200, 200);
        assert_eq!(after.len(), 2);
        let (t2, b2) = (
            after[0].bottom.as_ref().unwrap().hidden,
            after[1].top.as_ref().unwrap().hidden,
        );
        assert_eq!(t2, b2, "the two ends disagree: {t2} vs {b2}");
        assert_eq!(
            t2,
            top - 10,
            "ten lines were shown, so ten fewer are hidden"
        );
    }

    /// `crossed_*` is a count of hunks, and it saturates. The UI can only
    /// reach a count by pressing `z` on a boundary that offered the next hunk,
    /// so it never asks for more than exist — but that is the caller's habit,
    /// and `plan` is total regardless of it.
    #[test]
    fn crossing_further_than_there_are_hunks_saturates() {
        let a = entry(20, 2, 20, 2);
        let b = entry(30, 2, 30, 2);
        let hunks = [
            shown(0, &a),
            Candidate {
                index: 1,
                shown: false,
                entry: &b,
            },
        ];
        let absurd = HashMap::from([(
            0,
            Expansion {
                crossed_up: 999,
                crossed_down: 999,
                ..Expansion::default()
            },
        )]);
        let blocks = plan(&hunks, &absurd, 3, 100, 100);
        assert_eq!(blocks.len(), 1, "everything lands in one block");
        // Both hunks are in it, the far one marked foreign, and neither end
        // claims there is another hunk beyond.
        let changes: Vec<(usize, bool)> = blocks[0]
            .segments
            .iter()
            .filter_map(|s| match s {
                Segment::Change { hunk, foreign, .. } => Some((*hunk, *foreign)),
                _ => None,
            })
            .collect();
        assert_eq!(changes, vec![(0, false), (1, true)]);
        assert_eq!(blocks[0].top.as_ref().and_then(|b| b.next), None);
        assert_eq!(blocks[0].bottom.as_ref().and_then(|b| b.next), None);
    }

    #[test]
    fn crossing_costs_no_context_budget() {
        let a = entry(20, 2, 20, 2);
        let hidden = entry(26, 1, 26, 4);
        let hunks = [
            shown(0, &a),
            Candidate {
                index: 1,
                shown: false,
                entry: &hidden,
            },
        ];
        let crossed = HashMap::from([(
            0,
            Expansion {
                crossed_down: 1,
                down: 5,
                ..Expansion::default()
            },
        )]);
        let b = &plan(&hunks, &crossed, 3, 200, 203)[0];
        // The 5 extra lines land ENTIRELY beyond the crossed hunk: 3 + 5 = 8.
        assert_eq!(ctx(b.segments.last().unwrap()), (27, 30, 8));
    }

    /// Crossing everything leaves a plain file edge, and only then does the
    /// boundary disappear.
    #[test]
    fn crossing_all_the_way_to_the_file_edge_removes_the_boundary() {
        let a = entry(2, 1, 2, 1);
        let b = entry(5, 1, 5, 1);
        let hunks = [
            shown(0, &a),
            Candidate {
                index: 1,
                shown: false,
                entry: &b,
            },
        ];
        let all = HashMap::from([(
            0,
            Expansion {
                crossed_down: 1,
                down: 99,
                up: 99,
                ..Expansion::default()
            },
        )]);
        let block = &plan(&hunks, &all, 3, 8, 8)[0];
        assert!(block.top.is_none(), "line 1 is the top of the file");
        assert!(block.bottom.is_none(), "and line 8 is the end of it");
    }
}
