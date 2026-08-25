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
//! **A window never crosses another hunk.** Between two hunks the old/new line
//! offset is constant, which is what lets a context stretch carry both sides'
//! numbers from one length; across a hunk it is not. Stopping at the neighbour
//! keeps every rendered line number honest, and means expanding can never
//! quietly present someone else's change as untouched context.

use std::collections::HashMap;
use std::ops::Range;

use differential_engine::schema;

/// How far one hunk's context has been pulled open past the default, in lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Expansion {
    pub up: usize,
    pub down: usize,
}

/// Which end of a block a boundary row sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Up,
    Down,
}

/// A boundary row: press `z` here and `hunk` grows on `side`. `hidden` is how
/// many lines are still available in that direction — always more than zero,
/// because a boundary with nothing left behind it is not drawn at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary {
    pub hunk: usize,
    pub side: Side,
    pub hidden: usize,
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

    // Group the shown hunks into blocks: two of them join when they are
    // neighbours in the file AND their windows cover the whole gap between.
    let shown: Vec<usize> = (0..hunks.len()).filter(|&i| hunks[i].shown).collect();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for &i in &shown {
        let joins = groups
            .last()
            .and_then(|g| g.last().copied())
            .is_some_and(|prev| prev + 1 == i && want(prev).1 + want(i).0 >= avail_down(prev));
        if joins {
            groups
                .last_mut()
                .expect("joins implies a last group")
                .push(i);
        } else {
            groups.push(vec![i]);
        }
    }

    groups
        .into_iter()
        .map(|group| {
            let first = group[0];
            let last = *group.last().expect("a group is never empty");
            let (up_avail, down_avail) = (avail_up(first), avail_down(last));
            let up = want(first).0.min(up_avail);
            let down = want(last).1.min(down_avail);

            let mut segments = Vec::new();
            if up > 0 {
                let (old, new) = (&cuts[first].0, &cuts[first].1);
                segments.push(Segment::Context {
                    old_from: old.above_end - up,
                    new_from: new.above_end - up,
                    len: up,
                });
            }
            for (n, &i) in group.iter().enumerate() {
                if n > 0 {
                    // The gap to the previous hunk, which the merge condition
                    // guarantees is fully covered.
                    let (prev_old, prev_new) = (&cuts[group[n - 1]].0, &cuts[group[n - 1]].1);
                    let len = avail_down(group[n - 1]);
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
                    old: old.changed.clone(),
                    new: new.changed.clone(),
                });
            }
            if down > 0 {
                let (old, new) = (&cuts[last].0, &cuts[last].1);
                segments.push(Segment::Context {
                    old_from: old.below_start,
                    new_from: new.below_start,
                    len: down,
                });
            }

            Block {
                top: (up_avail > up).then(|| Boundary {
                    hunk: hunks[first].index,
                    side: Side::Up,
                    hidden: up_avail - up,
                }),
                segments,
                bottom: (down_avail > down).then(|| Boundary {
                    hunk: hunks[last].index,
                    side: Side::Down,
                    hidden: down_avail - down,
                }),
            }
        })
        .collect()
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
        assert_eq!(
            b.segments[1],
            Segment::Change {
                hunk: 7,
                old: 20..23,
                new: 20..24
            }
        );
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
        assert_eq!(
            b.segments[1],
            Segment::Change {
                hunk: 0,
                old: 5..5,
                new: 6..9
            }
        );
        assert_eq!(ctx(&b.segments[2]), (6, 9, 2));
    }

    /// `@@ -6,3 +5,0 @@`: the mirror image.
    #[test]
    fn a_pure_deletion_has_no_new_side_lines() {
        let e = entry(6, 3, 5, 0);
        let blocks = plan(&[shown(0, &e)], &HashMap::new(), 2, 53, 50);
        let b = &blocks[0];
        assert_eq!(ctx(&b.segments[0]), (4, 4, 2));
        assert_eq!(
            b.segments[1],
            Segment::Change {
                hunk: 0,
                old: 6..9,
                new: 5..5
            }
        );
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
        let far = HashMap::from([(0, Expansion { up: 0, down: 999 })]);
        let b = &plan(&[shown(0, &e)], &far, 3, 10, 10)[0];
        assert!(b.bottom.is_none());
        assert_eq!(ctx(b.segments.last().unwrap()), (3, 3, 8));
    }

    #[test]
    fn expansion_grows_one_side_only() {
        let e = entry(50, 2, 50, 2);
        let up = HashMap::from([(4, Expansion { up: 10, down: 0 })]);
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
        let merged = HashMap::from([(0, Expansion { up: 0, down: 8 })]);
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
                Segment::Change {
                    hunk: 0,
                    old: 20..22,
                    new: 20..22
                },
                Segment::Context {
                    old_from: 22,
                    new_from: 22,
                    len: 8
                },
                Segment::Change {
                    hunk: 1,
                    old: 30..32,
                    new: 30..32
                },
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
        assert_eq!(
            b.segments[0],
            Segment::Change {
                hunk: 0,
                old: 0..0,
                new: 1..2
            }
        );
        // Below, the old side resumes at line 1 while the new side is at 2.
        assert_eq!(ctx(&b.segments[1]), (1, 2, 3));

        // And asking for a hundred lines up cannot conjure any.
        let huge = HashMap::from([(0, Expansion { up: 100, down: 0 })]);
        assert!(plan(&[shown(0, &e)], &huge, 3, 4, 5)[0].top.is_none());
    }

    /// A deletion of the whole file: nothing on the new side at all.
    #[test]
    fn a_file_emptied_completely_still_plans() {
        let e = entry(1, 3, 0, 0);
        let b = &plan(&[shown(0, &e)], &HashMap::new(), 3, 3, 0)[0];
        assert!(b.top.is_none() && b.bottom.is_none());
        assert_eq!(
            b.segments,
            vec![Segment::Change {
                hunk: 0,
                old: 1..4,
                new: 0..0
            }]
        );
    }

    /// The reason a window stops at its neighbour: across a hunk the old/new
    /// offset changes, so lines past it are not shared context and cannot be
    /// numbered on both sides at once.
    #[test]
    fn a_window_never_crosses_an_unlisted_hunk() {
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
        // the unlisted hunk, and the two never merge.
        let huge = HashMap::from([
            (0, Expansion { up: 100, down: 100 }),
            (2, Expansion { up: 100, down: 100 }),
        ]);
        let blocks = plan(&hunks, &huge, 3, 200, 203);
        assert_eq!(blocks.len(), 2);
        // Below the first hunk: lines 22..25 only — four, up to `above_end` 26.
        assert_eq!(ctx(blocks[0].segments.last().unwrap()), (22, 22, 4));
        assert!(
            blocks[0].bottom.is_none(),
            "the gap is fully shown, so there is nothing left to unfold"
        );
        // Above the second: from the unlisted hunk's resume points, 27/30.
        assert_eq!(ctx(&blocks[1].segments[0]), (27, 30, 13));
        assert!(blocks[1].top.is_none());
        assert!(
            blocks[0]
                .segments
                .iter()
                .all(|s| !matches!(s, Segment::Change { hunk: 1, .. })),
            "an unlisted hunk must never render as a change"
        );
    }
}
