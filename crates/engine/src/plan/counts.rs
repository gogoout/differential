//! Added / removed line totals.

use crate::schema;

/// Line totals for a hunk, or any aggregation of hunks.
///
/// Canonical enumeration is `-U0`, so a hunk carries no context lines and the
/// counts are exactly its added and removed lines. Aggregating them was open-
/// coded in three places in the TUI alone, each re-deciding which schema field
/// meant which direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineCounts {
    pub adds: usize,
    pub dels: usize,
}

impl LineCounts {
    pub fn of_hunk(h: &schema::HunkEntry) -> Self {
        LineCounts {
            adds: h.new_count as usize,
            dels: h.old_count as usize,
        }
    }
}

impl std::ops::Add for LineCounts {
    type Output = LineCounts;

    fn add(self, rhs: LineCounts) -> LineCounts {
        LineCounts {
            adds: self.adds + rhs.adds,
            dels: self.dels + rhs.dels,
        }
    }
}

impl std::ops::AddAssign for LineCounts {
    fn add_assign(&mut self, rhs: LineCounts) {
        *self = *self + rhs;
    }
}

impl std::iter::Sum for LineCounts {
    fn sum<I: Iterator<Item = LineCounts>>(iter: I) -> LineCounts {
        iter.fold(LineCounts::default(), |a, b| a + b)
    }
}
