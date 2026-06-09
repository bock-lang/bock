//! `textDocument/rename` support — validation of candidate names.
//!
//! The edit set itself comes from [`crate::references::find_occurrences`];
//! this module owns the rule for whether a proposed new name is legal:
//!
//! - it must lex as a single Bock identifier (same character rules as the
//!   lexer: alphabetic or `_` first, alphanumeric or `_` after);
//! - it must not be a reserved keyword (`fn`, `let`, `match`, `Self`,
//!   `Ok`, …) or the bare wildcard `_`;
//! - it must keep the name's capitalization class. Bock's lexer assigns
//!   uppercase-initial names a different token kind (`TypeIdent`) than
//!   other names (`Ident`), so changing the class would change how every
//!   occurrence re-lexes and break the program.

use bock_lexer::keyword_lookup;
use thiserror::Error;

/// Why a proposed rename target is not a usable Bock name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RenameError {
    /// The new name does not lex as a single Bock identifier.
    #[error("`{0}` is not a valid Bock identifier")]
    InvalidIdentifier(String),
    /// The new name is a reserved keyword.
    #[error("`{0}` is a reserved Bock keyword and cannot be used as a name")]
    ReservedKeyword(String),
    /// The new name changes the capitalization class of the old name.
    /// Uppercase-initial names lex as type identifiers; everything else
    /// lexes as value identifiers — renaming across that boundary would
    /// change how every occurrence parses.
    #[error(
        "`{new}` does not match the capitalization of `{old}`: names starting \
         with an uppercase letter are type identifiers, all others are value \
         identifiers"
    )]
    CapitalizationMismatch {
        /// The symbol's current name.
        old: String,
        /// The rejected replacement.
        new: String,
    },
}

/// Validate `new_name` as a replacement for `old_name`.
///
/// # Errors
///
/// Returns a [`RenameError`] describing the first rule the candidate
/// violates; see the module docs for the rules.
pub fn validate_new_name(old_name: &str, new_name: &str) -> Result<(), RenameError> {
    if !is_identifier_shaped(new_name) {
        return Err(RenameError::InvalidIdentifier(new_name.to_string()));
    }
    if keyword_lookup(new_name).is_some() {
        return Err(RenameError::ReservedKeyword(new_name.to_string()));
    }
    if starts_uppercase(old_name) != starts_uppercase(new_name) {
        return Err(RenameError::CapitalizationMismatch {
            old: old_name.to_string(),
            new: new_name.to_string(),
        });
    }
    Ok(())
}

/// `true` if `name` lexes as one identifier token: alphabetic or `_` first,
/// alphanumeric or `_` after, and not the bare wildcard `_`.
fn is_identifier_shaped(name: &str) -> bool {
    if name == "_" {
        // `_` alone is the wildcard token, not an identifier.
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Capitalization class used by the lexer: uppercase-initial names lex as
/// `TypeIdent`, everything else as `Ident`.
fn starts_uppercase(name: &str) -> bool {
    name.starts_with(char::is_uppercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_lowercase_rename() {
        assert_eq!(validate_new_name("answer", "result"), Ok(()));
    }

    #[test]
    fn accepts_underscores_and_digits() {
        assert_eq!(validate_new_name("answer", "the_answer_42"), Ok(()));
        assert_eq!(validate_new_name("answer", "_private"), Ok(()));
    }

    #[test]
    fn accepts_uppercase_rename_for_uppercase_name() {
        assert_eq!(validate_new_name("Point", "Coordinate"), Ok(()));
    }

    #[test]
    fn rejects_keywords() {
        for kw in ["fn", "let", "mut", "match", "if", "true", "false"] {
            assert_eq!(
                validate_new_name("answer", kw),
                Err(RenameError::ReservedKeyword(kw.to_string())),
                "`{kw}` must be rejected",
            );
        }
    }

    #[test]
    fn rejects_constructor_keywords() {
        // `Ok` / `Some` / `Self` are keyword tokens in Bock's lexer, not
        // ordinary type identifiers.
        for kw in ["Ok", "Err", "Some", "None", "Self"] {
            assert_eq!(
                validate_new_name("Point", kw),
                Err(RenameError::ReservedKeyword(kw.to_string())),
                "`{kw}` must be rejected",
            );
        }
    }

    #[test]
    fn rejects_non_identifier_shapes() {
        for bad in ["", "_", "123abc", "foo-bar", "foo bar", "a.b", "x!"] {
            assert_eq!(
                validate_new_name("answer", bad),
                Err(RenameError::InvalidIdentifier(bad.to_string())),
                "`{bad}` must be rejected",
            );
        }
    }

    #[test]
    fn rejects_capitalization_class_change() {
        assert!(matches!(
            validate_new_name("Point", "point"),
            Err(RenameError::CapitalizationMismatch { .. }),
        ));
        assert!(matches!(
            validate_new_name("answer", "Answer"),
            Err(RenameError::CapitalizationMismatch { .. }),
        ));
    }

    #[test]
    fn error_messages_are_descriptive() {
        let err = validate_new_name("answer", "fn").expect_err("keyword rejected");
        assert!(err.to_string().contains("reserved"), "got: {err}");
        let err = validate_new_name("Point", "point").expect_err("case rejected");
        assert!(err.to_string().contains("capitalization"), "got: {err}");
    }
}
