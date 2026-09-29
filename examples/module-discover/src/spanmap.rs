//! Conversion of proc-macro2 line/column positions to byte offsets.
//!
//! With the `span-locations` feature, proc-macro2 reports positions as
//! 1-based line and 0-based *character* (Unicode scalar value) columns. The
//! discoverer reports byte ranges, so each loaded source keeps a table of the
//! byte offsets of its line starts.

use proc_macro2::LineColumn;

use crate::model::SpanRange;

pub struct LineMap {
    /// Byte offset at which each line starts; index 0 corresponds to line 1.
    line_starts: Vec<usize>,
}

impl LineMap {
    pub fn new(src: &str) -> Self {
        let mut line_starts = vec![0usize];
        let bytes = src.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\n' {
                line_starts.push(i + 1);
            }
            i += 1;
        }
        LineMap { line_starts }
    }

    /// Byte offset of a position. `line` is 1-based, `column` 0-based and
    /// measured in characters.
    pub fn offset(&self, src: &str, pos: LineColumn) -> usize {
        let line_idx = pos.line.saturating_sub(1).min(self.line_starts.len() - 1);
        let line_start = self.line_starts[line_idx];
        let line_end = self
            .line_starts
            .get(line_idx + 1)
            .copied()
            .unwrap_or(src.len());
        let line_text = &src[line_start..line_end];
        for (char_count, (byte_idx, _ch)) in line_text.char_indices().enumerate() {
            if char_count == pos.column {
                return line_start + byte_idx;
            }
        }
        line_start + line_text.len()
    }

    pub fn range(&self, src: &str, start: LineColumn, end: LineColumn) -> SpanRange {
        SpanRange {
            start: self.offset(src, start),
            end: self.offset(src, end),
        }
    }
}
