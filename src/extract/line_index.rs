//! Byte offset → line/column, in log(lines) rather than O(offset).
//!
//! rwr counts newlines in the prefix on every lookup, which is fine because it
//! only ever asks about a match. An extractor asks about every definition,
//! reference, and call in the file, so the prefix scan would be quadratic.

use crate::core::Pos;

pub(super) struct LineIndex {
    /// Byte offset of the first character of each line.
    starts: Vec<usize>,
}

impl LineIndex {
    pub(super) fn new(src: &[u8]) -> LineIndex {
        let mut starts = vec![0];
        starts.extend(
            src.iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i + 1),
        );
        LineIndex { starts }
    }

    /// Lines in the file. A trailing newline does not open a new line.
    pub(super) fn count(&self) -> usize {
        self.starts.len().saturating_sub(1).max(1)
    }

    /// 1-based line and column, as an editor and `file:line:col` mean them.
    pub(super) fn pos(&self, offset: usize) -> Pos {
        let line = self.starts.partition_point(|start| *start <= offset);
        let start = self.starts[line.saturating_sub(1)];
        Pos {
            line: line.max(1) as u32,
            col: (offset.saturating_sub(start) + 1) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_offsets_to_one_based_line_and_column() {
        let index = LineIndex::new(b"a\nbb\nccc");
        let at = |o| (index.pos(o).line, index.pos(o).col);
        assert_eq!(at(0), (1, 1));
        assert_eq!(at(2), (2, 1));
        assert_eq!(at(3), (2, 2));
        assert_eq!(at(5), (3, 1));
    }

    #[test]
    fn an_offset_past_the_end_still_lands_on_the_last_line() {
        let index = LineIndex::new(b"a\nb\n");
        assert_eq!(index.pos(99).line, 3);
    }
}
