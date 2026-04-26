//! Comment extraction from source text.
//!
//! Since regular comments (`//` and `/* */`) are discarded by the lexer,
//! the formatter extracts them directly from the source text to attempt
//! best-effort preservation.

/// A comment found in source text.
#[derive(Debug, Clone)]
pub struct Comment {
    /// Byte offset of the comment start in the source.
    pub start: usize,
    /// Byte offset past the comment end.
    pub end: usize,
    /// The comment text (including delimiters).
    pub text: String,
    /// The kind of comment.
    pub kind: CommentKind,
}

/// Classification of comments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    /// `//` line comment (not doc).
    Line,
    /// `/* ... */` block comment.
    Block,
    /// `///` doc comment — these are also in the AST.
    Doc,
    /// `//!` module doc comment — also in the AST.
    ModuleDoc,
}

/// Extract all comments from source text.
#[must_use]
pub fn extract_comments(source: &str) -> Vec<Comment> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut comments = Vec::new();
    let mut i = 0;

    while i < len {
        match bytes[i] {
            b'"' => {
                // Skip string literals
                i = skip_string(bytes, i);
            }
            b'/' if i + 1 < len => {
                match bytes[i + 1] {
                    b'/' => {
                        let start = i;
                        let kind = if i + 2 < len && bytes[i + 2] == b'/' {
                            CommentKind::Doc
                        } else if i + 2 < len && bytes[i + 2] == b'!' {
                            CommentKind::ModuleDoc
                        } else {
                            CommentKind::Line
                        };
                        // Advance to end of line
                        while i < len && bytes[i] != b'\n' {
                            i += 1;
                        }
                        let text = source[start..i].to_string();
                        comments.push(Comment {
                            start,
                            end: i,
                            text,
                            kind,
                        });
                    }
                    b'*' => {
                        let start = i;
                        i += 2;
                        let mut depth = 1u32;
                        while i < len && depth > 0 {
                            if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
                                depth += 1;
                                i += 2;
                            } else if bytes[i] == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                                depth -= 1;
                                i += 2;
                            } else {
                                i += 1;
                            }
                        }
                        let text = source[start..i].to_string();
                        comments.push(Comment {
                            start,
                            end: i,
                            text,
                            kind: CommentKind::Block,
                        });
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    comments
}

/// Skip past a string literal starting at position `i`.
fn skip_string(bytes: &[u8], mut i: usize) -> usize {
    let len = bytes.len();
    debug_assert!(bytes[i] == b'"');
    i += 1; // skip opening quote
    while i < len {
        match bytes[i] {
            b'\\' => {
                i += 2; // skip escape
            }
            b'"' => {
                i += 1; // skip closing quote
                return i;
            }
            _ => {
                i += 1;
            }
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_line_comment() {
        let src = "let x = 1 // a comment\nlet y = 2";
        let comments = extract_comments(src);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].kind, CommentKind::Line);
        assert_eq!(comments[0].text, "// a comment");
    }

    #[test]
    fn extract_doc_comment() {
        let src = "/// doc comment\nfn foo() {}";
        let comments = extract_comments(src);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].kind, CommentKind::Doc);
    }

    #[test]
    fn extract_module_doc() {
        let src = "//! module doc\n";
        let comments = extract_comments(src);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].kind, CommentKind::ModuleDoc);
    }

    #[test]
    fn extract_block_comment() {
        let src = "/* block */ foo";
        let comments = extract_comments(src);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].kind, CommentKind::Block);
        assert_eq!(comments[0].text, "/* block */");
    }

    #[test]
    fn skip_string_with_slash() {
        let src = r#"let x = "not // a comment""#;
        let comments = extract_comments(src);
        assert!(comments.is_empty());
    }

    #[test]
    fn nested_block_comment() {
        let src = "/* outer /* inner */ end */ foo";
        let comments = extract_comments(src);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].text, "/* outer /* inner */ end */");
    }
}
