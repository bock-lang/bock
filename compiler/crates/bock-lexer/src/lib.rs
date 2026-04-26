//! Bock lexer — tokenization of Bock source files into a token stream

mod lexer;
mod token;
pub mod vocab;

pub use lexer::Lexer;
pub use token::{keyword_lookup, Token, TokenKind};
