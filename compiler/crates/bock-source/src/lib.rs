//! Bock source — source file loading, span management, and file registry.
//!
//! [`Span`] and [`FileId`] are defined in `bock-errors` and re-exported here
//! so downstream crates can import them from a single convenient location.

use std::path::PathBuf;

pub use bock_errors::{FileId, Span};

// ─── SourceFile ───────────────────────────────────────────────────────────────

/// A loaded source file, keyed by its [`FileId`].
pub struct SourceFile {
    pub id: FileId,
    pub path: PathBuf,
    pub content: String,
    /// Byte offsets of the start of each line (line 0 starts at offset 0).
    line_starts: Vec<usize>,
}

impl SourceFile {
    /// Create a new [`SourceFile`], pre-computing line-start offsets.
    #[must_use]
    pub fn new(id: FileId, path: PathBuf, content: String) -> Self {
        let line_starts = std::iter::once(0)
            .chain(content.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        Self {
            id,
            path,
            content,
            line_starts,
        }
    }

    /// Returns `(line, column)`, both **1-indexed**.
    ///
    /// `offset` is a byte offset into the file. Column counts Unicode scalar
    /// values (characters), not bytes, from the start of the line.
    ///
    /// # Panics
    /// Panics if `offset` is beyond the end of the file.
    #[must_use]
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        assert!(offset <= self.content.len(), "offset out of range");

        // Binary-search for the last line_start <= offset.
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };

        let line_start = self.line_starts[line_idx];
        let col = self.content[line_start..offset].chars().count() + 1;
        (line_idx + 1, col)
    }

    /// Returns the textual content of the given 1-indexed line (without the
    /// trailing newline).
    ///
    /// # Panics
    /// Panics if `line` is 0 or beyond the last line.
    #[must_use]
    pub fn line_content(&self, line: usize) -> &str {
        assert!(line >= 1, "line must be 1-indexed");
        let idx = line - 1;
        let start = self.line_starts[idx];
        let end = self
            .line_starts
            .get(idx + 1)
            .map(|&s| {
                // Trim the '\n' that begins the next line's start marker.
                if s > 0 && self.content.as_bytes()[s - 1] == b'\n' {
                    s - 1
                } else {
                    s
                }
            })
            .unwrap_or(self.content.len());
        &self.content[start..end]
    }

    /// Returns the source text covered by `span`.
    ///
    /// # Panics
    /// Panics if `span` indices are out of bounds.
    #[must_use]
    pub fn slice(&self, span: Span) -> &str {
        &self.content[span.start..span.end]
    }
}

// ─── SourceMap ────────────────────────────────────────────────────────────────

/// Manages all source files loaded during a compilation session.
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    /// Create an empty [`SourceMap`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a file and return its assigned [`FileId`].
    pub fn add_file(&mut self, path: PathBuf, content: String) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile::new(id, path, content));
        id
    }

    /// Retrieve a file by its [`FileId`].
    ///
    /// # Panics
    /// Panics if `id` does not correspond to a registered file.
    #[must_use]
    pub fn get_file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file(content: &str) -> SourceFile {
        SourceFile::new(FileId(1), PathBuf::from("test.bock"), content.to_string())
    }

    // ── line_col ──────────────────────────────────────────────────────────────

    #[test]
    fn line_col_start_of_file() {
        let f = make_file("hello\nworld");
        assert_eq!(f.line_col(0), (1, 1));
    }

    #[test]
    fn line_col_end_of_first_line() {
        let f = make_file("hello\nworld");
        // offset 4 = 'o' in "hello"
        assert_eq!(f.line_col(4), (1, 5));
    }

    #[test]
    fn line_col_start_of_second_line() {
        let f = make_file("hello\nworld");
        // offset 6 = 'w' in "world"
        assert_eq!(f.line_col(6), (2, 1));
    }

    #[test]
    fn line_col_end_of_file() {
        let f = make_file("ab\ncd");
        assert_eq!(f.line_col(5), (2, 3));
    }

    #[test]
    fn line_col_single_line() {
        let f = make_file("abcde");
        assert_eq!(f.line_col(3), (1, 4));
    }

    // ── UTF-8 / multi-byte ────────────────────────────────────────────────────

    #[test]
    fn line_col_multibyte_char() {
        // "é" is 2 bytes (U+00E9), "x" is after it.
        let f = make_file("aéx");
        let e_offset = "a".len(); // 1
        let x_offset = "aé".len(); // 3
        assert_eq!(f.line_col(e_offset), (1, 2)); // col counts chars
        assert_eq!(f.line_col(x_offset), (1, 3));
    }

    #[test]
    fn line_col_emoji() {
        // "🦀" is 4 bytes; column should be 2, not 5.
        let f = make_file("a🦀b");
        let crab_offset = 1_usize;
        let b_offset = 1 + "🦀".len(); // 5
        assert_eq!(f.line_col(crab_offset), (1, 2));
        assert_eq!(f.line_col(b_offset), (1, 3));
    }

    #[test]
    fn line_col_multibyte_on_second_line() {
        let f = make_file("hello\nwörld");
        // 'ö' is 2 bytes, starts at offset 7
        let o_offset = "hello\nw".len(); // 7
        assert_eq!(f.line_col(o_offset), (2, 2));
    }

    // ── line_content ──────────────────────────────────────────────────────────

    #[test]
    fn line_content_first_line() {
        let f = make_file("hello\nworld");
        assert_eq!(f.line_content(1), "hello");
    }

    #[test]
    fn line_content_second_line() {
        let f = make_file("hello\nworld");
        assert_eq!(f.line_content(2), "world");
    }

    #[test]
    fn line_content_single_line_no_newline() {
        let f = make_file("only");
        assert_eq!(f.line_content(1), "only");
    }

    #[test]
    fn line_content_empty_line() {
        let f = make_file("a\n\nb");
        assert_eq!(f.line_content(2), "");
    }

    // ── slice ─────────────────────────────────────────────────────────────────

    #[test]
    fn slice_basic() {
        let f = make_file("hello world");
        let span = Span {
            file: FileId(1),
            start: 6,
            end: 11,
        };
        assert_eq!(f.slice(span), "world");
    }

    #[test]
    fn slice_whole_file() {
        let f = make_file("abc");
        let span = Span {
            file: FileId(1),
            start: 0,
            end: 3,
        };
        assert_eq!(f.slice(span), "abc");
    }

    #[test]
    fn slice_multibyte() {
        let f = make_file("a🦀b");
        let span = Span {
            file: FileId(1),
            start: 1,
            end: 5,
        };
        assert_eq!(f.slice(span), "🦀");
    }

    // ── SourceMap ─────────────────────────────────────────────────────────────

    #[test]
    fn source_map_add_and_get() {
        let mut map = SourceMap::new();
        let id = map.add_file(PathBuf::from("a.bock"), "fn main() {}".to_string());
        assert_eq!(id, FileId(0));
        let file = map.get_file(id);
        assert_eq!(file.content, "fn main() {}");
    }

    #[test]
    fn source_map_multiple_files() {
        let mut map = SourceMap::new();
        let id0 = map.add_file(PathBuf::from("a.bock"), "aaa".to_string());
        let id1 = map.add_file(PathBuf::from("b.bock"), "bbb".to_string());
        assert_eq!(id0, FileId(0));
        assert_eq!(id1, FileId(1));
        assert_eq!(map.get_file(id0).content, "aaa");
        assert_eq!(map.get_file(id1).content, "bbb");
    }

    #[test]
    fn source_map_file_id_matches() {
        let mut map = SourceMap::new();
        let id = map.add_file(PathBuf::from("x.bock"), "x".to_string());
        assert_eq!(map.get_file(id).id, id);
    }
}
