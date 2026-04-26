//! Token types for the Bock lexer.

use bock_source::Span;
use std::fmt;

/// The kind of a lexical token.
///
/// # Note on `Shr` vs `Compose`
/// Both `>>` and the function-composition operator `>>` share the same
/// source spelling. The lexer always emits `Shr`; the parser
/// re-interprets the token as `Compose` when it appears in an expression
/// context where the shift reading makes no sense.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[must_use]
pub enum TokenKind {
    // ── Literals ────────────────────────────────────────────────────────────
    /// Integer literal: `42`, `0xFF`, `0o77`, `0b1010`
    IntLiteral,
    /// Floating-point literal: `3.14`, `1.0e10`
    FloatLiteral,
    /// Standard string literal: `"hello"`
    StringLiteral,
    /// Raw string literal: `r"no escapes"`
    RawStringLiteral,
    /// Multi-line string literal: `"""..."""`
    MultiLineStringLiteral,
    /// Raw multi-line string literal: `r"""..."""`
    RawMultiLineStringLiteral,
    /// Character literal: `'a'`
    CharLiteral,
    /// Boolean literal: `true` / `false` (also produced as keywords)
    BoolLiteral,

    // ── String interpolation (produced by the string sub-lexer, P1.3) ──────
    /// A literal text segment between interpolation expressions.
    StringLiteralPart,
    /// `${` — starts an interpolated expression inside a string.
    InterpolationStart,
    /// `}` — closes an interpolated expression inside a string.
    InterpolationEnd,

    // ── Identifiers ─────────────────────────────────────────────────────────
    /// Lowercase/underscore identifier: `foo`, `_bar`
    Ident,
    /// Type identifier (starts with uppercase): `Foo`, `MyType`
    TypeIdent,

    // ── Keywords ────────────────────────────────────────────────────────────
    Fn,
    Let,
    Mut,
    Const,
    If,
    Else,
    Match,
    For,
    In,
    While,
    Loop,
    Break,
    Continue,
    Return,
    Guard,
    With,
    Handling,
    Handle,
    Record,
    Enum,
    Class,
    Trait,
    Impl,
    /// `self` (lowercase)
    SelfLower,
    /// `Self` (uppercase)
    SelfUpper,
    Module,
    Use,
    Public,
    Internal,
    Native,
    Async,
    Await,
    Effect,
    Platform,
    Where,
    Type,
    /// `Ok` — standard result variant keyword
    Ok_,
    /// `Err` — standard result variant keyword
    Err_,
    /// `Some` — standard option variant keyword
    Some_,
    /// `None` — standard option variant keyword
    None_,
    Property,
    Forall,
    Unreachable,
    /// `is` — type-test / pattern keyword
    Is,

    // ── Arithmetic operators ─────────────────────────────────────────────────
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `**`
    Power,

    // ── Comparison operators ─────────────────────────────────────────────────
    /// `==`
    Eq,
    /// `!=`
    Neq,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    Lte,
    /// `>=`
    Gte,

    // ── Logical / bitwise operators ──────────────────────────────────────────
    /// `&&`
    And,
    /// `||`
    Or,
    /// `!`
    Not,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
    /// `~`
    BitNot,
    /// `<<`
    Shl,
    /// `>>` — lexer always emits this; parser re-interprets as `Compose` when needed.
    Shr,
    /// `>>` in function-composition context — never emitted by the lexer directly;
    /// the parser re-interprets `Shr` as `Compose` in expression position.
    Compose,

    // ── Assignment operators ─────────────────────────────────────────────────
    /// `=`
    Assign,
    /// `+=`
    PlusEq,
    /// `-=`
    MinusEq,
    /// `*=`
    StarEq,
    /// `/=`
    SlashEq,
    /// `%=`
    PercentEq,

    // ── Special operators / punctuation ──────────────────────────────────────
    /// `|>` — pipe operator
    Pipe,
    /// `=>` — fat arrow (match arms, lambdas)
    FatArrow,
    /// `->` — thin arrow (return-type annotation)
    ThinArrow,
    /// `?` — error propagation / optional chaining
    Question,
    /// `..` — exclusive range
    DotDot,
    /// `..=` — inclusive range
    DotDotEq,
    /// `.`
    Dot,
    /// `_` — wildcard / placeholder
    Underscore,
    /// `#` — attribute sigil
    Hash,
    /// `@`
    At,

    // ── Delimiters ───────────────────────────────────────────────────────────
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `;`
    Semicolon,

    // ── Special ──────────────────────────────────────────────────────────────
    /// Significant newline (statement terminator)
    Newline,
    /// `///` doc comment
    DocComment,
    /// `//!` module doc comment
    ModuleDocComment,
    /// End of file
    Eof,
    /// Lexer error token
    Error,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TokenKind::IntLiteral => "<int>",
            TokenKind::FloatLiteral => "<float>",
            TokenKind::StringLiteral => "<string>",
            TokenKind::RawStringLiteral => "<raw-string>",
            TokenKind::MultiLineStringLiteral => "<multiline-string>",
            TokenKind::RawMultiLineStringLiteral => "<raw-multiline-string>",
            TokenKind::CharLiteral => "<char>",
            TokenKind::BoolLiteral => "<bool>",
            TokenKind::StringLiteralPart => "<string-part>",
            TokenKind::InterpolationStart => "${",
            TokenKind::InterpolationEnd => "}",
            TokenKind::Ident => "<ident>",
            TokenKind::TypeIdent => "<type-ident>",
            TokenKind::Fn => "fn",
            TokenKind::Let => "let",
            TokenKind::Mut => "mut",
            TokenKind::Const => "const",
            TokenKind::If => "if",
            TokenKind::Else => "else",
            TokenKind::Match => "match",
            TokenKind::For => "for",
            TokenKind::In => "in",
            TokenKind::While => "while",
            TokenKind::Loop => "loop",
            TokenKind::Break => "break",
            TokenKind::Continue => "continue",
            TokenKind::Return => "return",
            TokenKind::Guard => "guard",
            TokenKind::With => "with",
            TokenKind::Handling => "handling",
            TokenKind::Handle => "handle",
            TokenKind::Record => "record",
            TokenKind::Enum => "enum",
            TokenKind::Class => "class",
            TokenKind::Trait => "trait",
            TokenKind::Impl => "impl",
            TokenKind::SelfLower => "self",
            TokenKind::SelfUpper => "Self",
            TokenKind::Module => "module",
            TokenKind::Use => "use",
            TokenKind::Public => "public",
            TokenKind::Internal => "internal",
            TokenKind::Native => "native",
            TokenKind::Async => "async",
            TokenKind::Await => "await",
            TokenKind::Effect => "effect",
            TokenKind::Platform => "platform",
            TokenKind::Where => "where",
            TokenKind::Type => "type",
            TokenKind::Ok_ => "Ok",
            TokenKind::Err_ => "Err",
            TokenKind::Some_ => "Some",
            TokenKind::None_ => "None",
            TokenKind::Property => "property",
            TokenKind::Forall => "forall",
            TokenKind::Unreachable => "unreachable",
            TokenKind::Is => "is",
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            TokenKind::Power => "**",
            TokenKind::Eq => "==",
            TokenKind::Neq => "!=",
            TokenKind::Lt => "<",
            TokenKind::Gt => ">",
            TokenKind::Lte => "<=",
            TokenKind::Gte => ">=",
            TokenKind::And => "&&",
            TokenKind::Or => "||",
            TokenKind::Not => "!",
            TokenKind::BitAnd => "&",
            TokenKind::BitOr => "|",
            TokenKind::BitXor => "^",
            TokenKind::BitNot => "~",
            TokenKind::Shl => "<<",
            TokenKind::Shr => ">>",
            TokenKind::Compose => ">>",
            TokenKind::Assign => "=",
            TokenKind::PlusEq => "+=",
            TokenKind::MinusEq => "-=",
            TokenKind::StarEq => "*=",
            TokenKind::SlashEq => "/=",
            TokenKind::PercentEq => "%=",
            TokenKind::Pipe => "|>",
            TokenKind::FatArrow => "=>",
            TokenKind::ThinArrow => "->",
            TokenKind::Question => "?",
            TokenKind::DotDot => "..",
            TokenKind::DotDotEq => "..=",
            TokenKind::Dot => ".",
            TokenKind::Underscore => "_",
            TokenKind::Hash => "#",
            TokenKind::At => "@",
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::LBracket => "[",
            TokenKind::RBracket => "]",
            TokenKind::LBrace => "{",
            TokenKind::RBrace => "}",
            TokenKind::Comma => ",",
            TokenKind::Colon => ":",
            TokenKind::Semicolon => ";",
            TokenKind::Newline => "<newline>",
            TokenKind::DocComment => "///",
            TokenKind::ModuleDocComment => "//!",
            TokenKind::Eof => "<eof>",
            TokenKind::Error => "<error>",
        };
        f.write_str(s)
    }
}

/// A single lexical token with its kind, source span, and optional literal text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct Token {
    /// The syntactic kind of this token.
    pub kind: TokenKind,
    /// Source location.
    pub span: Span,
    /// Raw literal content for tokens where the text is significant
    /// (string content, numeric literals, identifiers, comments, errors).
    pub literal: Option<String>,
}

impl Token {
    /// Construct a new token.
    pub fn new(kind: TokenKind, span: Span, literal: Option<String>) -> Self {
        Self {
            kind,
            span,
            literal,
        }
    }
}

/// Map a source identifier string to its keyword [`TokenKind`], if any.
///
/// Returns `None` for ordinary identifiers.
#[must_use]
pub fn keyword_lookup(ident: &str) -> Option<TokenKind> {
    let kind = match ident {
        "fn" => TokenKind::Fn,
        "let" => TokenKind::Let,
        "mut" => TokenKind::Mut,
        "const" => TokenKind::Const,
        "if" => TokenKind::If,
        "else" => TokenKind::Else,
        "match" => TokenKind::Match,
        "for" => TokenKind::For,
        "in" => TokenKind::In,
        "while" => TokenKind::While,
        "loop" => TokenKind::Loop,
        "break" => TokenKind::Break,
        "continue" => TokenKind::Continue,
        "return" => TokenKind::Return,
        "guard" => TokenKind::Guard,
        "with" => TokenKind::With,
        "handling" => TokenKind::Handling,
        "handle" => TokenKind::Handle,
        "record" => TokenKind::Record,
        "enum" => TokenKind::Enum,
        "class" => TokenKind::Class,
        "trait" => TokenKind::Trait,
        "impl" => TokenKind::Impl,
        "self" => TokenKind::SelfLower,
        "Self" => TokenKind::SelfUpper,
        "module" => TokenKind::Module,
        "use" => TokenKind::Use,
        "public" => TokenKind::Public,
        "internal" => TokenKind::Internal,
        "native" => TokenKind::Native,
        "async" => TokenKind::Async,
        "await" => TokenKind::Await,
        "effect" => TokenKind::Effect,
        "platform" => TokenKind::Platform,
        "where" => TokenKind::Where,
        "type" => TokenKind::Type,
        "true" => TokenKind::BoolLiteral,
        "false" => TokenKind::BoolLiteral,
        "Ok" => TokenKind::Ok_,
        "Err" => TokenKind::Err_,
        "Some" => TokenKind::Some_,
        "None" => TokenKind::None_,
        "property" => TokenKind::Property,
        "forall" => TokenKind::Forall,
        "unreachable" => TokenKind::Unreachable,
        "is" => TokenKind::Is,
        _ => return None,
    };
    Some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_errors::Span;

    fn dummy_span() -> Span {
        Span::dummy()
    }

    #[test]
    fn keyword_lookup_known() {
        assert_eq!(keyword_lookup("fn"), Some(TokenKind::Fn));
        assert_eq!(keyword_lookup("let"), Some(TokenKind::Let));
        assert_eq!(keyword_lookup("mut"), Some(TokenKind::Mut));
        assert_eq!(keyword_lookup("true"), Some(TokenKind::BoolLiteral));
        assert_eq!(keyword_lookup("false"), Some(TokenKind::BoolLiteral));
        assert_eq!(keyword_lookup("self"), Some(TokenKind::SelfLower));
        assert_eq!(keyword_lookup("Self"), Some(TokenKind::SelfUpper));
        assert_eq!(keyword_lookup("Ok"), Some(TokenKind::Ok_));
        assert_eq!(keyword_lookup("Err"), Some(TokenKind::Err_));
        assert_eq!(keyword_lookup("Some"), Some(TokenKind::Some_));
        assert_eq!(keyword_lookup("None"), Some(TokenKind::None_));
        assert_eq!(keyword_lookup("is"), Some(TokenKind::Is));
        assert_eq!(keyword_lookup("forall"), Some(TokenKind::Forall));
        assert_eq!(keyword_lookup("unreachable"), Some(TokenKind::Unreachable));
    }

    #[test]
    fn keyword_lookup_unknown() {
        assert_eq!(keyword_lookup("foo"), None);
        assert_eq!(keyword_lookup("Foo"), None);
        assert_eq!(keyword_lookup(""), None);
        assert_eq!(keyword_lookup("FN"), None);
        assert_eq!(keyword_lookup("Fn"), None);
    }

    #[test]
    fn display_operators() {
        assert_eq!(TokenKind::FatArrow.to_string(), "=>");
        assert_eq!(TokenKind::ThinArrow.to_string(), "->");
        assert_eq!(TokenKind::Pipe.to_string(), "|>");
        assert_eq!(TokenKind::Power.to_string(), "**");
        assert_eq!(TokenKind::Shr.to_string(), ">>");
        assert_eq!(TokenKind::DotDotEq.to_string(), "..=");
    }

    #[test]
    fn display_keywords() {
        assert_eq!(TokenKind::Fn.to_string(), "fn");
        assert_eq!(TokenKind::SelfLower.to_string(), "self");
        assert_eq!(TokenKind::SelfUpper.to_string(), "Self");
        assert_eq!(TokenKind::Ok_.to_string(), "Ok");
        assert_eq!(TokenKind::BoolLiteral.to_string(), "<bool>");
    }

    #[test]
    fn display_special() {
        assert_eq!(TokenKind::Eof.to_string(), "<eof>");
        assert_eq!(TokenKind::Newline.to_string(), "<newline>");
        assert_eq!(TokenKind::Error.to_string(), "<error>");
        assert_eq!(TokenKind::InterpolationStart.to_string(), "${");
        assert_eq!(TokenKind::InterpolationEnd.to_string(), "}");
    }

    #[test]
    fn token_construction() {
        let span = dummy_span();
        let tok = Token::new(TokenKind::Ident, span, Some("hello".into()));
        assert_eq!(tok.kind, TokenKind::Ident);
        assert_eq!(tok.literal.as_deref(), Some("hello"));
    }

    #[test]
    fn all_keywords_round_trip() {
        // Every keyword's Display output should re-lookup to itself.
        let keywords = [
            TokenKind::Fn,
            TokenKind::Let,
            TokenKind::Mut,
            TokenKind::Const,
            TokenKind::If,
            TokenKind::Else,
            TokenKind::Match,
            TokenKind::For,
            TokenKind::In,
            TokenKind::While,
            TokenKind::Loop,
            TokenKind::Break,
            TokenKind::Continue,
            TokenKind::Return,
            TokenKind::Guard,
            TokenKind::With,
            TokenKind::Handling,
            TokenKind::Handle,
            TokenKind::Record,
            TokenKind::Enum,
            TokenKind::Class,
            TokenKind::Trait,
            TokenKind::Impl,
            TokenKind::SelfLower,
            TokenKind::SelfUpper,
            TokenKind::Module,
            TokenKind::Use,
            TokenKind::Public,
            TokenKind::Internal,
            TokenKind::Native,
            TokenKind::Async,
            TokenKind::Await,
            TokenKind::Effect,
            TokenKind::Platform,
            TokenKind::Where,
            TokenKind::Type,
            TokenKind::Ok_,
            TokenKind::Err_,
            TokenKind::Some_,
            TokenKind::None_,
            TokenKind::Property,
            TokenKind::Forall,
            TokenKind::Unreachable,
            TokenKind::Is,
        ];
        for kw in &keywords {
            let text = kw.to_string();
            assert_eq!(
                keyword_lookup(&text).as_ref(),
                Some(kw),
                "round-trip failed for {kw:?} (display = {text:?})"
            );
        }
    }
}
