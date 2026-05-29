//! Introspection APIs for the lexer vocabulary.
//!
//! Exposes the full keyword and operator inventories so that tooling
//! (editor extensions, documentation generators) can render them
//! without re-hardcoding the lists.

/// A keyword entry as seen by the lexer.
pub struct KeywordInfo {
    /// The keyword text as written in source (e.g. `"fn"`).
    pub text: &'static str,
    /// Semantic category (e.g. `"control-flow"`, `"declaration"`).
    pub category: &'static str,
    /// Spec section reference, if any (e.g. `"§1.4"`).
    pub spec_ref: Option<&'static str>,
}

/// An operator entry as seen by the lexer.
pub struct OperatorInfo {
    /// The operator symbol (e.g. `"+"`, `">>"`).
    pub symbol: &'static str,
    /// Precedence level 1..=15 (Bock spec §5.1). `None` for delimiters.
    pub precedence: Option<u8>,
    /// Associativity — `"left"`, `"right"`, or `"none"`.
    pub associativity: &'static str,
    /// Short human-readable label.
    pub kind: &'static str,
    /// Spec section reference, if any.
    pub spec_ref: Option<&'static str>,
}

/// The complete list of Bock keywords.
#[must_use]
pub fn keywords() -> Vec<KeywordInfo> {
    vec![
        KeywordInfo {
            text: "fn",
            category: "declaration",
            spec_ref: Some("§1.4"),
        },
        KeywordInfo {
            text: "let",
            category: "declaration",
            spec_ref: Some("§1.4"),
        },
        KeywordInfo {
            text: "mut",
            category: "modifier",
            spec_ref: Some("§3"),
        },
        KeywordInfo {
            text: "const",
            category: "declaration",
            spec_ref: Some("§1.4"),
        },
        KeywordInfo {
            text: "if",
            category: "control-flow",
            spec_ref: Some("§5"),
        },
        KeywordInfo {
            text: "else",
            category: "control-flow",
            spec_ref: Some("§5"),
        },
        KeywordInfo {
            text: "match",
            category: "control-flow",
            spec_ref: Some("§7"),
        },
        KeywordInfo {
            text: "for",
            category: "control-flow",
            spec_ref: Some("§6"),
        },
        KeywordInfo {
            text: "in",
            category: "control-flow",
            spec_ref: Some("§6"),
        },
        KeywordInfo {
            text: "while",
            category: "control-flow",
            spec_ref: Some("§6"),
        },
        KeywordInfo {
            text: "loop",
            category: "control-flow",
            spec_ref: Some("§6"),
        },
        KeywordInfo {
            text: "break",
            category: "control-flow",
            spec_ref: Some("§6"),
        },
        KeywordInfo {
            text: "continue",
            category: "control-flow",
            spec_ref: Some("§6"),
        },
        KeywordInfo {
            text: "return",
            category: "control-flow",
            spec_ref: Some("§6"),
        },
        KeywordInfo {
            text: "guard",
            category: "control-flow",
            spec_ref: Some("§6"),
        },
        KeywordInfo {
            text: "with",
            category: "effects",
            spec_ref: Some("§8"),
        },
        KeywordInfo {
            text: "handling",
            category: "effects",
            spec_ref: Some("§8"),
        },
        KeywordInfo {
            text: "handle",
            category: "effects",
            spec_ref: Some("§8"),
        },
        KeywordInfo {
            text: "record",
            category: "declaration",
            spec_ref: Some("§4"),
        },
        KeywordInfo {
            text: "enum",
            category: "declaration",
            spec_ref: Some("§4"),
        },
        KeywordInfo {
            text: "class",
            category: "declaration",
            spec_ref: Some("§4"),
        },
        KeywordInfo {
            text: "trait",
            category: "declaration",
            spec_ref: Some("§4"),
        },
        KeywordInfo {
            text: "impl",
            category: "declaration",
            spec_ref: Some("§4"),
        },
        KeywordInfo {
            text: "self",
            category: "keyword",
            spec_ref: Some("§4"),
        },
        KeywordInfo {
            text: "Self",
            category: "type",
            spec_ref: Some("§4"),
        },
        KeywordInfo {
            text: "module",
            category: "declaration",
            spec_ref: Some("§10"),
        },
        KeywordInfo {
            text: "use",
            category: "declaration",
            spec_ref: Some("§10"),
        },
        KeywordInfo {
            text: "public",
            category: "visibility",
            spec_ref: Some("§10"),
        },
        KeywordInfo {
            text: "internal",
            category: "visibility",
            spec_ref: Some("§10"),
        },
        KeywordInfo {
            text: "native",
            category: "modifier",
            spec_ref: Some("§13"),
        },
        KeywordInfo {
            text: "async",
            category: "effects",
            spec_ref: Some("§8"),
        },
        KeywordInfo {
            text: "await",
            category: "effects",
            spec_ref: Some("§8"),
        },
        KeywordInfo {
            text: "effect",
            category: "effects",
            spec_ref: Some("§8"),
        },
        KeywordInfo {
            text: "platform",
            category: "declaration",
            spec_ref: Some("§13"),
        },
        KeywordInfo {
            text: "where",
            category: "declaration",
            spec_ref: Some("§4"),
        },
        KeywordInfo {
            text: "type",
            category: "declaration",
            spec_ref: Some("§4"),
        },
        KeywordInfo {
            text: "true",
            category: "literal",
            spec_ref: Some("§1.3"),
        },
        KeywordInfo {
            text: "false",
            category: "literal",
            spec_ref: Some("§1.3"),
        },
        KeywordInfo {
            text: "Ok",
            category: "constructor",
            spec_ref: Some("§6.3"),
        },
        KeywordInfo {
            text: "Err",
            category: "constructor",
            spec_ref: Some("§6.3"),
        },
        KeywordInfo {
            text: "Some",
            category: "constructor",
            spec_ref: Some("§6.3"),
        },
        KeywordInfo {
            text: "None",
            category: "constructor",
            spec_ref: Some("§6.3"),
        },
        KeywordInfo {
            text: "property",
            category: "declaration",
            spec_ref: Some("§4"),
        },
        KeywordInfo {
            text: "forall",
            category: "type",
            spec_ref: Some("§4"),
        },
        KeywordInfo {
            text: "unreachable",
            category: "keyword",
            spec_ref: Some("§6"),
        },
        KeywordInfo {
            text: "is",
            category: "operator",
            spec_ref: Some("§7"),
        },
    ]
}

/// The complete list of Bock operators and punctuation.
#[must_use]
pub fn operators() -> Vec<OperatorInfo> {
    vec![
        // Assignment — precedence 1
        OperatorInfo {
            symbol: "=",
            precedence: Some(1),
            associativity: "right",
            kind: "assignment",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "+=",
            precedence: Some(1),
            associativity: "right",
            kind: "assignment",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "-=",
            precedence: Some(1),
            associativity: "right",
            kind: "assignment",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "*=",
            precedence: Some(1),
            associativity: "right",
            kind: "assignment",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "/=",
            precedence: Some(1),
            associativity: "right",
            kind: "assignment",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "%=",
            precedence: Some(1),
            associativity: "right",
            kind: "assignment",
            spec_ref: Some("§5.1"),
        },
        // Pipe — precedence 2
        OperatorInfo {
            symbol: "|>",
            precedence: Some(2),
            associativity: "left",
            kind: "pipe",
            spec_ref: Some("§5.1"),
        },
        // Compose — precedence 3
        OperatorInfo {
            symbol: ">>",
            precedence: Some(3),
            associativity: "left",
            kind: "compose",
            spec_ref: Some("§5.1"),
        },
        // Range — precedence 4
        OperatorInfo {
            symbol: "..",
            precedence: Some(4),
            associativity: "none",
            kind: "range",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "..=",
            precedence: Some(4),
            associativity: "none",
            kind: "range",
            spec_ref: Some("§5.1"),
        },
        // Logical OR — precedence 5
        OperatorInfo {
            symbol: "||",
            precedence: Some(5),
            associativity: "left",
            kind: "logical",
            spec_ref: Some("§5.1"),
        },
        // Logical AND — precedence 6
        OperatorInfo {
            symbol: "&&",
            precedence: Some(6),
            associativity: "left",
            kind: "logical",
            spec_ref: Some("§5.1"),
        },
        // Comparison — precedence 7
        OperatorInfo {
            symbol: "==",
            precedence: Some(7),
            associativity: "none",
            kind: "comparison",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "!=",
            precedence: Some(7),
            associativity: "none",
            kind: "comparison",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "<",
            precedence: Some(7),
            associativity: "none",
            kind: "comparison",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: ">",
            precedence: Some(7),
            associativity: "none",
            kind: "comparison",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "<=",
            precedence: Some(7),
            associativity: "none",
            kind: "comparison",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: ">=",
            precedence: Some(7),
            associativity: "none",
            kind: "comparison",
            spec_ref: Some("§5.1"),
        },
        // Bitwise OR — precedence 8
        OperatorInfo {
            symbol: "|",
            precedence: Some(8),
            associativity: "left",
            kind: "bitwise",
            spec_ref: Some("§5.1"),
        },
        // Bitwise XOR — precedence 9
        OperatorInfo {
            symbol: "^",
            precedence: Some(9),
            associativity: "left",
            kind: "bitwise",
            spec_ref: Some("§5.1"),
        },
        // Bitwise AND — precedence 10
        OperatorInfo {
            symbol: "&",
            precedence: Some(10),
            associativity: "left",
            kind: "bitwise",
            spec_ref: Some("§5.1"),
        },
        // Additive — precedence 11
        OperatorInfo {
            symbol: "+",
            precedence: Some(11),
            associativity: "left",
            kind: "arithmetic",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "-",
            precedence: Some(11),
            associativity: "left",
            kind: "arithmetic",
            spec_ref: Some("§5.1"),
        },
        // Multiplicative — precedence 12
        OperatorInfo {
            symbol: "*",
            precedence: Some(12),
            associativity: "left",
            kind: "arithmetic",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "/",
            precedence: Some(12),
            associativity: "left",
            kind: "arithmetic",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "%",
            precedence: Some(12),
            associativity: "left",
            kind: "arithmetic",
            spec_ref: Some("§5.1"),
        },
        // Power — precedence 13
        OperatorInfo {
            symbol: "**",
            precedence: Some(13),
            associativity: "right",
            kind: "arithmetic",
            spec_ref: Some("§5.1"),
        },
        // Unary — precedence 14
        OperatorInfo {
            symbol: "!",
            precedence: Some(14),
            associativity: "right",
            kind: "unary",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: "~",
            precedence: Some(14),
            associativity: "right",
            kind: "unary",
            spec_ref: Some("§5.1"),
        },
        // Postfix — precedence 15
        OperatorInfo {
            symbol: "?",
            precedence: Some(15),
            associativity: "left",
            kind: "postfix",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: ".",
            precedence: Some(15),
            associativity: "left",
            kind: "access",
            spec_ref: Some("§5.1"),
        },
        // Delimiters & punctuation
        OperatorInfo {
            symbol: "=>",
            precedence: None,
            associativity: "none",
            kind: "punctuation",
            spec_ref: None,
        },
        OperatorInfo {
            symbol: "->",
            precedence: None,
            associativity: "none",
            kind: "punctuation",
            spec_ref: None,
        },
        OperatorInfo {
            symbol: "<<",
            precedence: Some(10),
            associativity: "left",
            kind: "bitwise",
            spec_ref: Some("§5.1"),
        },
        OperatorInfo {
            symbol: ":",
            precedence: None,
            associativity: "none",
            kind: "punctuation",
            spec_ref: None,
        },
        OperatorInfo {
            symbol: ";",
            precedence: None,
            associativity: "none",
            kind: "punctuation",
            spec_ref: None,
        },
        OperatorInfo {
            symbol: ",",
            precedence: None,
            associativity: "none",
            kind: "punctuation",
            spec_ref: None,
        },
        OperatorInfo {
            symbol: "@",
            precedence: None,
            associativity: "none",
            kind: "annotation",
            spec_ref: None,
        },
        OperatorInfo {
            symbol: "#",
            precedence: None,
            associativity: "none",
            kind: "attribute",
            spec_ref: None,
        },
        OperatorInfo {
            symbol: "_",
            precedence: None,
            associativity: "none",
            kind: "wildcard",
            spec_ref: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_keyword_round_trips_through_lookup() {
        for kw in keywords() {
            assert!(
                crate::keyword_lookup(kw.text).is_some(),
                "keyword {:?} not in keyword_lookup",
                kw.text
            );
        }
    }

    #[test]
    fn keywords_are_non_empty() {
        assert!(!keywords().is_empty());
    }

    #[test]
    fn operators_are_non_empty() {
        assert!(!operators().is_empty());
    }

    #[test]
    fn operator_precedence_in_range() {
        for op in operators() {
            if let Some(p) = op.precedence {
                assert!((1..=15).contains(&p), "op {:?} precedence {}", op.symbol, p);
            }
        }
    }
}
