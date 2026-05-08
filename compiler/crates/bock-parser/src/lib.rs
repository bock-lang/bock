//! Bock parser — transforms a token stream into a typed abstract syntax tree.
//!
//! # Usage
//! ```ignore
//! let parser = Parser::new(tokens, &source_file);
//! let module = parser.parse_module();
//! ```

use bock_ast::{
    Annotation, AnnotationArg, Arg, AssignOp, AssociatedType, BinOp, Block, ClassDecl, ConstDecl,
    EffectDecl, EnumDecl, EnumVariant, Expr, FnDecl, ForLoop, GenericParam, GuardStmt, HandlerPair,
    HandlingBlock, Ident, ImplBlock, ImportDecl, ImportItems, ImportedName, InterpolationPart,
    Item, LetStmt, Literal, MatchArm, Module, ModuleHandleDecl, ModulePath, NodeId, Param, Pattern,
    RecordDecl, RecordDeclField, RecordField, RecordPatternField, RecordSpread, Stmt, TraitDecl,
    TypeAliasDecl, TypeConstraint, TypeExpr, TypePath, UnaryOp, Visibility, WhileLoop,
};
use bock_errors::{DiagnosticBag, DiagnosticCode, Span};
use bock_lexer::{Token, TokenKind};
use bock_source::SourceFile;

/// Internal tag for binary operators during precedence climbing.
#[derive(Debug, Clone, PartialEq)]
enum OpTag {
    Assign(AssignOp),
    Pipe,
    Compose,
    Range,
    RangeInclusive,
    Binary(BinOp),
    Is,
}

/// The Bock parser. Transforms a flat token stream into a typed [`Module`] AST.
///
/// Create with [`Parser::new`], then call [`Parser::parse_module`].
pub struct Parser<'src> {
    tokens: Vec<Token>,
    pos: usize,
    source: &'src SourceFile,
    diagnostics: DiagnosticBag,
    next_id: NodeId,
    /// Tracks consecutive parse errors for panic-mode recovery.
    consecutive_errors: usize,
}

impl<'src> Parser<'src> {
    /// Create a new parser from a token stream and its source file.
    ///
    /// The token stream must contain at least one token (the `Eof` sentinel).
    #[must_use]
    pub fn new(tokens: Vec<Token>, source: &'src SourceFile) -> Self {
        assert!(
            !tokens.is_empty(),
            "token list must contain at least an EOF token"
        );
        Self {
            tokens,
            pos: 0,
            source,
            diagnostics: DiagnosticBag::new(),
            next_id: 0,
            consecutive_errors: 0,
        }
    }

    /// Parse the full token stream as a source file, returning the root [`Module`].
    pub fn parse_module(&mut self) -> Module {
        let start_span = self.peek().span;

        // Collect module-level doc comments (`//!`).
        // Skip leading newlines so `//!` is found even after regular `//` comments.
        self.skip_newlines();
        let mut doc = Vec::new();
        while self.at(TokenKind::ModuleDocComment) {
            let tok = self.advance();
            if let Some(text) = tok.literal {
                doc.push(text);
            }
            self.skip_newlines();
        }
        self.skip_newlines();

        // Optional `module path.name` declaration.
        let path = if self.at(TokenKind::Module) {
            Some(self.parse_module_decl())
        } else {
            None
        };
        self.skip_newlines();

        // Collect module-level doc comments (`//!`) that appear after the
        // module declaration.  The spec allows `//!` both before and after
        // `module name`, so we absorb them here too.
        while self.at(TokenKind::ModuleDocComment) {
            let tok = self.advance();
            if let Some(text) = tok.literal {
                doc.push(text);
            }
            self.skip_newlines();
        }
        self.skip_newlines();

        // Import declarations (`use ...` and `public use ...`).
        let mut imports = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(TokenKind::Use) {
                imports.push(self.parse_import_decl(Visibility::Private));
            } else if self.at_visibility() && self.peek_kind_at(1) == Some(TokenKind::Use) {
                let vis = self.parse_visibility();
                imports.push(self.parse_import_decl(vis));
            } else {
                break;
            }
        }
        self.skip_newlines();

        // Top-level items.
        let items = self.parse_items();

        let end_span = self.peek().span;
        Module {
            id: self.alloc_id(),
            span: Span::merge(start_span, end_span),
            doc,
            path,
            imports,
            items,
        }
    }

    /// Returns a reference to the accumulated diagnostics.
    #[must_use]
    pub fn diagnostics(&self) -> &DiagnosticBag {
        &self.diagnostics
    }

    // ─── Token-stream primitives ──────────────────────────────────────────────

    /// Look at the current token without consuming it.
    pub(crate) fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    /// Consume and return the current token, advancing the cursor.
    pub(crate) fn advance(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    /// Returns `true` if the current token has the given `kind`.
    #[must_use]
    pub(crate) fn at(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    /// Consume the current token if it matches `kind`; otherwise emit an error diagnostic.
    pub(crate) fn expect(&mut self, kind: TokenKind) -> Result<Token, ()> {
        if self.at(kind.clone()) {
            Ok(self.advance())
        } else {
            let span = self.peek().span;
            let found = self.peek().kind.clone();
            self.diagnostics.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 2000,
                },
                format!("expected `{kind}`, found `{found}`"),
                span,
            );
            Err(())
        }
    }

    /// Skip over any [`TokenKind::Newline`] tokens.
    pub(crate) fn skip_newlines(&mut self) {
        while self.at(TokenKind::Newline) {
            let _ = self.advance();
        }
    }

    /// Return the kind of the first non-newline token at or after the cursor,
    /// without advancing.
    fn peek_past_newlines_kind(&self) -> Option<TokenKind> {
        let mut i = self.pos;
        while i < self.tokens.len() && self.tokens[i].kind == TokenKind::Newline {
            i += 1;
        }
        self.tokens.get(i).map(|t| t.kind.clone())
    }

    // ─── Private helpers ──────────────────────────────────────────────────────

    fn alloc_id(&mut self) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Peek at the kind of the token `offset` positions ahead of the cursor.
    fn peek_kind_at(&self, offset: usize) -> Option<TokenKind> {
        self.tokens.get(self.pos + offset).map(|t| t.kind.clone())
    }

    fn at_visibility(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Public | TokenKind::Internal)
    }

    /// Returns `true` if the current token starts a top-level declaration.
    fn at_decl_start(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Fn
                | TokenKind::Async
                | TokenKind::Record
                | TokenKind::Enum
                | TokenKind::Class
                | TokenKind::Trait
                | TokenKind::Platform
                | TokenKind::Impl
                | TokenKind::Effect
                | TokenKind::Const
                | TokenKind::Type
                | TokenKind::Use
                | TokenKind::Handle
                | TokenKind::At
                | TokenKind::Public
                | TokenKind::Internal
        )
    }

    /// Synchronize the parser after an error by skipping tokens until a safe
    /// restart point: a declaration-starting keyword, `}`, `;`, or EOF.
    ///
    /// Returns the span of all tokens skipped.
    fn synchronize(&mut self) -> Span {
        let start = self.peek().span;
        let mut end = start;
        while !self.at(TokenKind::Eof) {
            let kind = self.peek().kind.clone();
            match kind {
                // Synchronization points — stop before consuming
                TokenKind::Semicolon | TokenKind::RBrace => {
                    let _ = self.advance(); // consume the sync token
                    end = self.peek().span;
                    break;
                }
                TokenKind::Newline => {
                    let _ = self.advance();
                    // If the next non-newline is a decl start, stop here
                    if self.at_decl_start() || self.at(TokenKind::Eof) {
                        break;
                    }
                }
                _ if self.at_decl_start() => break,
                _ => {
                    end = self.peek().span;
                    let _ = self.advance();
                }
            }
        }
        Span::merge(start, end)
    }

    /// Panic-mode recovery: skip all tokens until the next top-level declaration
    /// keyword (or EOF), consuming any intermediate braces/semicolons too.
    ///
    /// Used after 3+ consecutive errors to skip aggressively to the next item.
    fn synchronize_to_top_level(&mut self) -> Span {
        let start = self.peek().span;
        let mut end = start;
        while !self.at(TokenKind::Eof) {
            self.skip_newlines();
            if self.at(TokenKind::Eof) || self.at_decl_start() {
                break;
            }
            end = self.peek().span;
            let _ = self.advance();
        }
        Span::merge(start, end)
    }

    fn parse_visibility(&mut self) -> Visibility {
        match self.peek().kind {
            TokenKind::Public => {
                let _ = self.advance();
                Visibility::Public
            }
            TokenKind::Internal => {
                let _ = self.advance();
                Visibility::Internal
            }
            _ => Visibility::Private,
        }
    }

    // ─── Module declaration ───────────────────────────────────────────────────

    /// Parse `module path.name NEWLINE`.
    fn parse_module_decl(&mut self) -> ModulePath {
        let _ = self.advance(); // consume `module`
        let path = self.parse_module_path();
        if self.at(TokenKind::Newline) {
            let _ = self.advance();
        }
        path
    }

    /// Parse a dot-separated module path: `a.b.Name`.
    fn parse_module_path(&mut self) -> ModulePath {
        let start = self.peek().span;
        let mut segments = Vec::new();

        if let Some(seg) = self.try_parse_path_segment() {
            segments.push(seg);
        }

        // Continue consuming `.segment` pairs.
        while self.at(TokenKind::Dot) {
            match self.peek_kind_at(1) {
                Some(TokenKind::Ident) | Some(TokenKind::TypeIdent) => {
                    let _ = self.advance(); // consume `.`
                    if let Some(seg) = self.try_parse_path_segment() {
                        segments.push(seg);
                    }
                }
                _ => break,
            }
        }

        let end = segments.last().map(|s| s.span).unwrap_or(start);
        ModulePath {
            span: Span::merge(start, end),
            segments,
        }
    }

    /// Try to consume a single path segment (Ident or TypeIdent); emit an error on failure.
    fn try_parse_path_segment(&mut self) -> Option<Ident> {
        if matches!(self.peek().kind, TokenKind::Ident | TokenKind::TypeIdent) {
            let tok = self.advance();
            Some(Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            })
        } else {
            let span = self.peek().span;
            let found = self.peek().kind.clone();
            self.diagnostics.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 2001,
                },
                format!("expected identifier in path, found `{found}`"),
                span,
            );
            None
        }
    }

    // ─── Import declarations ──────────────────────────────────────────────────

    /// Parse `use module_path [import_list] NEWLINE`.
    fn parse_import_decl(&mut self, vis: Visibility) -> ImportDecl {
        let start = self.peek().span;
        let _ = self.advance(); // consume `use`

        // Parse the base module path, stopping before `.{` and `.*`.
        let path = self.parse_import_base_path();

        // Parse the optional import list.
        let items = self.parse_import_items();

        if self.at(TokenKind::Newline) {
            let _ = self.advance();
        }

        let end = self.peek().span;
        ImportDecl {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            visibility: vis,
            path,
            items,
        }
    }

    /// Parse the base path in a `use` declaration.
    ///
    /// Greedy, but stops before `.{` and `.*` so the caller can handle import lists.
    fn parse_import_base_path(&mut self) -> ModulePath {
        let start = self.peek().span;
        let mut segments = Vec::new();

        if let Some(seg) = self.try_parse_path_segment() {
            segments.push(seg);
        }

        while self.at(TokenKind::Dot) {
            match self.peek_kind_at(1) {
                // Stop: import list follows.
                Some(TokenKind::LBrace) | Some(TokenKind::Star) => break,
                // Continue consuming path segments.
                Some(TokenKind::Ident) | Some(TokenKind::TypeIdent) => {
                    let _ = self.advance(); // consume `.`
                    if let Some(seg) = self.try_parse_path_segment() {
                        segments.push(seg);
                    }
                }
                _ => break,
            }
        }

        let end = segments.last().map(|s| s.span).unwrap_or(start);
        ModulePath {
            span: Span::merge(start, end),
            segments,
        }
    }

    /// Parse the optional import list after the base path.
    ///
    /// | Syntax       | Result                  |
    /// |-------------|-------------------------|
    /// | `.{A, B}`   | `Named([A, B])`         |
    /// | `.*`        | `Glob`                  |
    /// | `.Name`     | `Named([Name])`         |
    /// | *(nothing)* | `Module`                |
    fn parse_import_items(&mut self) -> ImportItems {
        if !self.at(TokenKind::Dot) {
            return ImportItems::Module;
        }

        match self.peek_kind_at(1) {
            Some(TokenKind::Star) => {
                let _ = self.advance(); // `.`
                let _ = self.advance(); // `*`
                ImportItems::Glob
            }
            Some(TokenKind::LBrace) => {
                let _ = self.advance(); // `.`
                let _ = self.advance(); // `{`
                let names = self.parse_named_import_list();
                let _ = self.expect(TokenKind::RBrace);
                ImportItems::Named(names)
            }
            Some(TokenKind::Ident) | Some(TokenKind::TypeIdent) => {
                let _ = self.advance(); // `.`
                let tok = self.advance(); // the name
                let span = tok.span;
                let name = Ident {
                    name: tok.literal.unwrap_or_default(),
                    span,
                };
                ImportItems::Named(vec![ImportedName {
                    span,
                    name,
                    alias: None,
                }])
            }
            _ => ImportItems::Module,
        }
    }

    /// Parse comma-separated names inside `{...}`.
    fn parse_named_import_list(&mut self) -> Vec<ImportedName> {
        let mut names = Vec::new();
        self.skip_newlines();

        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if !matches!(self.peek().kind, TokenKind::Ident | TokenKind::TypeIdent) {
                break;
            }
            let tok = self.advance();
            let start_span = tok.span;
            let name = Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            };

            // Optional alias: `Name as Alias`.
            let alias = if self.at(TokenKind::Ident) && self.peek().literal.as_deref() == Some("as")
            {
                let _ = self.advance(); // consume `as`
                if matches!(self.peek().kind, TokenKind::Ident | TokenKind::TypeIdent) {
                    let alias_tok = self.advance();
                    Some(Ident {
                        name: alias_tok.literal.unwrap_or_default(),
                        span: alias_tok.span,
                    })
                } else {
                    None
                }
            } else {
                None
            };

            let end_span = alias.as_ref().map(|a| a.span).unwrap_or(start_span);
            names.push(ImportedName {
                span: Span::merge(start_span, end_span),
                name,
                alias,
            });

            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }

        names
    }

    // ─── Top-level items ──────────────────────────────────────────────────────

    /// Parse top-level items, dispatching to declaration-specific parsers.
    fn parse_items(&mut self) -> Vec<Item> {
        let mut items = Vec::new();

        loop {
            self.skip_newlines();
            if self.at(TokenKind::Eof) {
                break;
            }

            // Skip item-level doc comments (`///`).
            // The lexer produces DocComment tokens but the AST item types
            // don't store them yet, so consume and discard for now.
            while self.at(TokenKind::DocComment) || self.at(TokenKind::ModuleDocComment) {
                let _ = self.advance();
                self.skip_newlines();
            }
            if self.at(TokenKind::Eof) {
                break;
            }

            // Collect leading annotations.
            let mut annotations = Vec::new();
            while self.at(TokenKind::At) {
                annotations.push(self.parse_annotation());
                self.skip_newlines();
            }

            // Optional visibility modifier.
            let vis = if self.at_visibility() {
                self.parse_visibility()
            } else {
                Visibility::Private
            };

            let error_count_before = self.diagnostics.error_count();

            // Dispatch to the correct declaration parser.
            let item = match self.peek().kind.clone() {
                TokenKind::Fn | TokenKind::Async => Item::Fn(self.parse_fn_decl(annotations, vis)),
                TokenKind::Record => Item::Record(self.parse_record_decl(annotations, vis)),
                TokenKind::Enum => Item::Enum(self.parse_enum_decl(annotations, vis)),
                TokenKind::Class => Item::Class(self.parse_class_decl(annotations, vis)),
                TokenKind::Trait => Item::Trait(self.parse_trait_decl(annotations, vis, false)),
                TokenKind::Platform => {
                    // `platform trait Name ...`
                    Item::PlatformTrait(self.parse_platform_trait_decl(annotations, vis))
                }
                TokenKind::Impl => Item::Impl(self.parse_impl_block(annotations)),
                TokenKind::Effect => Item::Effect(self.parse_effect_decl(annotations, vis)),
                TokenKind::Handle => Item::ModuleHandle(self.parse_module_handle_decl()),
                TokenKind::Type => Item::TypeAlias(self.parse_type_alias_decl(annotations, vis)),
                TokenKind::Const => Item::Const(self.parse_const_decl(annotations, vis)),
                _ => {
                    if self.at(TokenKind::Eof) {
                        break;
                    }
                    // Unrecognized token at top level — emit error and recover.
                    let span = self.peek().span;
                    let found = self.peek().kind.clone();
                    self.diagnostics.error(
                        DiagnosticCode {
                            prefix: 'E',
                            number: 2050,
                        },
                        format!("unexpected token `{found}` at top level"),
                        span,
                    );
                    self.consecutive_errors += 1;
                    let error_span = if self.consecutive_errors >= 3 {
                        // Panic mode: skip to next top-level item
                        self.consecutive_errors = 0;
                        self.synchronize_to_top_level()
                    } else {
                        self.synchronize()
                    };
                    let id = self.alloc_id();
                    items.push(Item::Error {
                        id,
                        span: error_span,
                    });
                    continue;
                }
            };

            // Reset consecutive error count on successful parse.
            if self.diagnostics.error_count() == error_count_before {
                self.consecutive_errors = 0;
            } else {
                self.consecutive_errors += 1;
            }

            items.push(item);
        }

        items
    }

    // ─── Annotations ─────────────────────────────────────────────────────────

    /// Parse a single `@name(args)` annotation.
    fn parse_annotation(&mut self) -> Annotation {
        let start = self.peek().span;
        let _ = self.advance(); // consume `@`

        let name_span = self.peek().span;
        let name = if matches!(self.peek().kind, TokenKind::Ident | TokenKind::TypeIdent) {
            let tok = self.advance();
            Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            }
        } else {
            self.diagnostics.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 2002,
                },
                format!("expected annotation name, found `{}`", self.peek().kind),
                name_span,
            );
            Ident {
                name: String::new(),
                span: name_span,
            }
        };

        let mut args = Vec::new();
        if self.at(TokenKind::LParen) {
            let _ = self.advance(); // consume `(`
            self.skip_newlines();
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                // Handle named arguments `key: value` — capture the label.
                let label = if self.at(TokenKind::Ident)
                    && self.peek_kind_at(1) == Some(TokenKind::Colon)
                {
                    let lbl_tok = self.advance();
                    let _ = self.advance(); // consume `:`
                    Some(Ident {
                        name: lbl_tok.literal.unwrap_or_default(),
                        span: lbl_tok.span,
                    })
                } else {
                    None
                };
                args.push(AnnotationArg {
                    label,
                    value: self.parse_expr_stub(),
                });
                self.skip_newlines();
                if self.at(TokenKind::Comma) {
                    let _ = self.advance();
                    self.skip_newlines();
                } else {
                    break;
                }
            }
            let _ = self.expect(TokenKind::RParen);
        }

        let end = self.peek().span;
        Annotation {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            name,
            args,
        }
    }

    // ─── Generic parameters ───────────────────────────────────────────────────

    /// Parse `[T, U: Bound, ...]` generic parameter list.
    fn parse_generic_params(&mut self) -> Vec<GenericParam> {
        if !self.at(TokenKind::LBracket) {
            return Vec::new();
        }
        let _ = self.advance(); // consume `[`

        let mut params = Vec::new();
        self.skip_newlines();

        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
            if !matches!(self.peek().kind, TokenKind::Ident | TokenKind::TypeIdent) {
                break;
            }
            let id = self.alloc_id();
            let start = self.peek().span;
            let tok = self.advance();
            let name = Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            };

            // Optional bounds: `T: Bound1`.
            let bounds = if self.at(TokenKind::Colon) {
                let _ = self.advance();
                vec![self.parse_type_path()]
            } else {
                Vec::new()
            };

            let end = bounds.last().map(|b| b.span).unwrap_or(name.span);
            params.push(GenericParam {
                id,
                span: Span::merge(start, end),
                name,
                bounds,
            });

            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }

        let _ = self.expect(TokenKind::RBracket);
        params
    }

    /// Parse `where (T: Bound, U: Bound2, ...)` constraint list.
    fn parse_where_clause(&mut self) -> Vec<TypeConstraint> {
        if !self.at(TokenKind::Where) {
            return Vec::new();
        }
        let _ = self.advance(); // consume `where`

        let _ = self.expect(TokenKind::LParen);
        let mut constraints = Vec::new();
        self.skip_newlines();

        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            if !matches!(self.peek().kind, TokenKind::Ident | TokenKind::TypeIdent) {
                break;
            }
            let id = self.alloc_id();
            let start = self.peek().span;
            let tok = self.advance();
            let param = Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            };

            let _ = self.expect(TokenKind::Colon);
            let bounds = vec![self.parse_type_path()];

            let end = bounds.last().map(|b| b.span).unwrap_or(param.span);
            constraints.push(TypeConstraint {
                id,
                span: Span::merge(start, end),
                param,
                bounds,
            });

            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }

        let _ = self.expect(TokenKind::RParen);
        constraints
    }

    // ─── Type expressions ─────────────────────────────────────────────────────

    /// Parse a type expression.
    fn parse_type_expr(&mut self) -> TypeExpr {
        let id = self.alloc_id();
        let start = self.peek().span;

        let base = match self.peek().kind.clone() {
            TokenKind::LParen => {
                let _ = self.advance(); // consume `(`
                self.skip_newlines();

                if self.at(TokenKind::RParen) {
                    // Unit type `()`.
                    let end = self.advance().span;
                    TypeExpr::Tuple {
                        id,
                        span: Span::merge(start, end),
                        elems: vec![],
                    }
                } else {
                    let mut elems = Vec::new();
                    elems.push(self.parse_type_expr());
                    self.skip_newlines();
                    while self.at(TokenKind::Comma) {
                        let _ = self.advance();
                        self.skip_newlines();
                        if self.at(TokenKind::RParen) {
                            break;
                        }
                        elems.push(self.parse_type_expr());
                        self.skip_newlines();
                    }
                    let end = self
                        .expect(TokenKind::RParen)
                        .map(|t| t.span)
                        .unwrap_or(start);

                    // Check for function type arrow.
                    if self.at(TokenKind::ThinArrow) {
                        let _ = self.advance();
                        let ret = self.parse_type_expr();
                        TypeExpr::Function {
                            id,
                            span: Span::merge(start, ret.span()),
                            params: elems,
                            ret: Box::new(ret),
                            effects: vec![],
                        }
                    } else if elems.len() == 1 {
                        // Parenthesised single type — unwrap.
                        elems.remove(0)
                    } else {
                        TypeExpr::Tuple {
                            id,
                            span: Span::merge(start, end),
                            elems,
                        }
                    }
                }
            }

            TokenKind::SelfLower | TokenKind::SelfUpper => {
                let tok = self.advance();
                TypeExpr::SelfType { id, span: tok.span }
            }

            TokenKind::Ident | TokenKind::TypeIdent => {
                // `Fn(...)` — function type using the `Fn` keyword.
                if self.peek().literal.as_deref() == Some("Fn")
                    && self.peek_kind_at(1) == Some(TokenKind::LParen)
                {
                    let _ = self.advance(); // consume `Fn`
                    let _ = self.advance(); // consume `(`
                    self.skip_newlines();
                    let mut params = Vec::new();
                    while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                        params.push(self.parse_type_expr());
                        self.skip_newlines();
                        if self.at(TokenKind::Comma) {
                            let _ = self.advance();
                            self.skip_newlines();
                        } else {
                            break;
                        }
                    }
                    let _ = self.expect(TokenKind::RParen);
                    let _ = self.expect(TokenKind::ThinArrow);
                    let ret = self.parse_type_expr();
                    // Optional effect clause: `with TypePath, TypePath`.
                    let effects = self.parse_effect_clause();
                    let end = effects
                        .last()
                        .map(|e: &TypePath| e.span)
                        .unwrap_or(ret.span());
                    TypeExpr::Function {
                        id,
                        span: Span::merge(start, end),
                        params,
                        ret: Box::new(ret),
                        effects,
                    }
                } else {
                    let path = self.parse_type_path();
                    // Optional generic arguments `[T, U]`.
                    let args = if self.at(TokenKind::LBracket) {
                        let _ = self.advance();
                        let mut args = Vec::new();
                        self.skip_newlines();
                        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                            args.push(self.parse_type_expr());
                            self.skip_newlines();
                            if self.at(TokenKind::Comma) {
                                let _ = self.advance();
                                self.skip_newlines();
                            } else {
                                break;
                            }
                        }
                        let _ = self.expect(TokenKind::RBracket);
                        args
                    } else {
                        Vec::new()
                    };
                    let span = path.span;
                    TypeExpr::Named {
                        id,
                        span,
                        path,
                        args,
                    }
                }
            }

            _ => {
                self.diagnostics.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 2010,
                    },
                    format!("expected type expression, found `{}`", self.peek().kind),
                    start,
                );
                TypeExpr::Named {
                    id,
                    span: start,
                    path: TypePath {
                        segments: vec![],
                        span: start,
                    },
                    args: vec![],
                }
            }
        };

        // Postfix `?` for optional types.
        if self.at(TokenKind::Question) {
            let q = self.advance();
            let id2 = self.alloc_id();
            TypeExpr::Optional {
                id: id2,
                span: Span::merge(base.span(), q.span),
                inner: Box::new(base),
            }
        } else {
            base
        }
    }

    /// Parse a dot-separated type path: `Std.Io.File`.
    fn parse_type_path(&mut self) -> TypePath {
        let start = self.peek().span;
        let mut segments = Vec::new();

        if matches!(self.peek().kind, TokenKind::Ident | TokenKind::TypeIdent) {
            let tok = self.advance();
            segments.push(Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            });
        }

        while self.at(TokenKind::Dot) {
            match self.peek_kind_at(1) {
                Some(TokenKind::TypeIdent) | Some(TokenKind::Ident) => {
                    let _ = self.advance(); // consume `.`
                    let tok = self.advance();
                    segments.push(Ident {
                        name: tok.literal.unwrap_or_default(),
                        span: tok.span,
                    });
                }
                _ => break,
            }
        }

        let end = segments.last().map(|s| s.span).unwrap_or(start);
        TypePath {
            segments,
            span: Span::merge(start, end),
        }
    }

    // ─── Expression parsing (Pratt / precedence climbing) ────────────────────

    /// Parse a full expression.
    pub(crate) fn parse_expr(&mut self) -> Expr {
        self.parse_prec(0)
    }

    /// Kept as an alias so all existing call-sites continue to work.
    fn parse_expr_stub(&mut self) -> Expr {
        self.parse_expr()
    }

    /// Precedence climbing: parse an expression whose top-level binary operator
    /// has precedence ≥ `min_prec`.
    fn parse_prec(&mut self, min_prec: u8) -> Expr {
        // Parse the left-hand side (unary + postfix).
        let mut left = self.parse_unary();

        // Track whether we've consumed a comparison operator at this level.
        // Comparisons are non-associative (`a == b == c` is rejected), but
        // lower-precedence operators like `&&`/`||` must still be accepted
        // after a comparison (e.g. `a == 0 && b == 0`).
        let mut seen_comparison = false;

        loop {
            // Continuation rule: if the current token is a newline and the next
            // non-newline token is `|>` (Pipe), allow the expression to continue
            // on the next line.
            if self.at(TokenKind::Newline) {
                match self.peek_past_newlines_kind() {
                    Some(TokenKind::Pipe) => self.skip_newlines(),
                    _ => break,
                }
            }

            let Some((op_prec, right_prec, op_tok)) = self.binary_op_info() else {
                break;
            };
            if op_prec < min_prec {
                break;
            }

            // Non-associative comparison: reject a second comparison at this level.
            if op_prec == 7 {
                if seen_comparison {
                    break;
                }
                seen_comparison = true;
            }

            // Special case: `is` — RHS is a type expression, not a value expression.
            if op_tok == OpTag::Is {
                let _ = self.advance(); // consume `is`
                self.skip_newlines(); // allow RHS on next line after operator
                let ty = self.parse_type_expr();
                let id = self.alloc_id();
                let span = Span::merge(left.span(), ty.span());
                left = Expr::Is {
                    id,
                    span,
                    expr: Box::new(left),
                    type_expr: ty,
                };
                continue; // non-associativity handled by seen_comparison flag
            }

            // Special case: range operators (non-associative).
            if matches!(op_tok, OpTag::Range | OpTag::RangeInclusive) {
                let inclusive = op_tok == OpTag::RangeInclusive;
                let _ = self.advance(); // consume `..` or `..=`
                self.skip_newlines(); // allow RHS on next line after operator
                let right = self.parse_prec(op_prec + 1);
                let id = self.alloc_id();
                let span = Span::merge(left.span(), right.span());
                left = Expr::Range {
                    id,
                    span,
                    lo: Box::new(left),
                    hi: Box::new(right),
                    inclusive,
                };
                break; // non-associative
            }

            let _ = self.advance(); // consume the operator
                                    // Continuation rule: operator at end of line — skip newlines before RHS.
            self.skip_newlines();

            let right = self.parse_prec(right_prec);
            let id = self.alloc_id();
            let span = Span::merge(left.span(), right.span());

            left = match op_tok {
                OpTag::Assign(op) => Expr::Assign {
                    id,
                    span,
                    op,
                    target: Box::new(left),
                    value: Box::new(right),
                },
                OpTag::Pipe => Expr::Pipe {
                    id,
                    span,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                OpTag::Compose => Expr::Compose {
                    id,
                    span,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                OpTag::Binary(op) => Expr::Binary {
                    id,
                    span,
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                OpTag::Is | OpTag::Range | OpTag::RangeInclusive => unreachable!(),
            };

            // Non-associativity for comparisons is handled by the
            // seen_comparison flag at the top of the loop.
        }

        left
    }

    /// Returns `(op_precedence, right_min_precedence, op_tag)` for the current token,
    /// or `None` if it's not a binary operator.
    fn binary_op_info(&self) -> Option<(u8, u8, OpTag)> {
        // Precedence levels (15 total):
        //  1 = Assignment (right-assoc)
        //  2 = Pipe (left-assoc)
        //  3 = Compose >> (left-assoc)
        //  4 = Range .. ..= (non-assoc)
        //  5 = Or ||
        //  6 = And &&
        //  7 = Compare == != < > <= >= is (non-assoc)
        //  8 = BitOr |
        //  9 = BitXor ^
        // 10 = BitAnd &
        // 11 = Add + -
        // 12 = Mul * / %
        // 13 = Power ** (right-assoc)
        // 14 = Unary (prefix)
        // 15 = Postfix (call, index, member)
        let kind = &self.peek().kind;
        let (prec, right_prec, tag) = match kind {
            // Assignment — right-associative → right_prec = same level
            TokenKind::Assign => (1, 1, OpTag::Assign(AssignOp::Assign)),
            TokenKind::PlusEq => (1, 1, OpTag::Assign(AssignOp::AddAssign)),
            TokenKind::MinusEq => (1, 1, OpTag::Assign(AssignOp::SubAssign)),
            TokenKind::StarEq => (1, 1, OpTag::Assign(AssignOp::MulAssign)),
            TokenKind::SlashEq => (1, 1, OpTag::Assign(AssignOp::DivAssign)),
            TokenKind::PercentEq => (1, 1, OpTag::Assign(AssignOp::RemAssign)),
            // Pipe — left-assoc
            TokenKind::Pipe => (2, 3, OpTag::Pipe),
            // Compose (`>>` / Shr token) — left-assoc
            TokenKind::Shr | TokenKind::Compose => (3, 4, OpTag::Compose),
            // Range — non-associative
            TokenKind::DotDot => (4, 5, OpTag::Range),
            TokenKind::DotDotEq => (4, 5, OpTag::RangeInclusive),
            // Logical or
            TokenKind::Or => (5, 6, OpTag::Binary(BinOp::Or)),
            // Logical and
            TokenKind::And => (6, 7, OpTag::Binary(BinOp::And)),
            // Comparison — non-associative (right_prec > prec forces stop after one)
            TokenKind::Eq => (7, 8, OpTag::Binary(BinOp::Eq)),
            TokenKind::Neq => (7, 8, OpTag::Binary(BinOp::Ne)),
            TokenKind::Lt => (7, 8, OpTag::Binary(BinOp::Lt)),
            TokenKind::Gt => (7, 8, OpTag::Binary(BinOp::Gt)),
            TokenKind::Lte => (7, 8, OpTag::Binary(BinOp::Le)),
            TokenKind::Gte => (7, 8, OpTag::Binary(BinOp::Ge)),
            TokenKind::Is => (7, 8, OpTag::Is),
            // Bitwise
            TokenKind::BitOr => (8, 9, OpTag::Binary(BinOp::BitOr)),
            TokenKind::BitXor => (9, 10, OpTag::Binary(BinOp::BitXor)),
            TokenKind::BitAnd => (10, 11, OpTag::Binary(BinOp::BitAnd)),
            // Add / Sub
            TokenKind::Plus => (11, 12, OpTag::Binary(BinOp::Add)),
            TokenKind::Minus => (11, 12, OpTag::Binary(BinOp::Sub)),
            // Mul / Div / Rem
            TokenKind::Star => (12, 13, OpTag::Binary(BinOp::Mul)),
            TokenKind::Slash => (12, 13, OpTag::Binary(BinOp::Div)),
            TokenKind::Percent => (12, 13, OpTag::Binary(BinOp::Rem)),
            // Power — right-associative
            TokenKind::Power => (13, 13, OpTag::Binary(BinOp::Pow)),
            _ => return None,
        };
        Some((prec, right_prec, tag))
    }

    /// Parse unary prefix operators, then delegate to postfix chain.
    fn parse_unary(&mut self) -> Expr {
        let id = self.alloc_id();
        let span = self.peek().span;

        match self.peek().kind.clone() {
            TokenKind::Minus => {
                let _ = self.advance();
                let operand = self.parse_unary();
                let span = Span::merge(span, operand.span());
                Expr::Unary {
                    id,
                    span,
                    op: UnaryOp::Neg,
                    operand: Box::new(operand),
                }
            }
            TokenKind::Not => {
                let _ = self.advance();
                let operand = self.parse_unary();
                let span = Span::merge(span, operand.span());
                Expr::Unary {
                    id,
                    span,
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                }
            }
            TokenKind::BitNot => {
                let _ = self.advance();
                let operand = self.parse_unary();
                let span = Span::merge(span, operand.span());
                Expr::Unary {
                    id,
                    span,
                    op: UnaryOp::BitNot,
                    operand: Box::new(operand),
                }
            }
            _ => self.parse_postfix(),
        }
    }

    /// Parse a primary expression then apply postfix operators in a loop.
    fn parse_postfix(&mut self) -> Expr {
        let mut expr = self.parse_primary();

        loop {
            // Continuation rule: next line starts with `.` → allow method/field chaining
            // across lines (e.g., `expr\n  .method()`).
            if self.at(TokenKind::Newline) {
                match self.peek_past_newlines_kind() {
                    Some(TokenKind::Dot) => self.skip_newlines(),
                    _ => break,
                }
            }

            match self.peek().kind.clone() {
                // `?` — error propagation
                TokenKind::Question => {
                    let end_span = self.advance().span;
                    let id = self.alloc_id();
                    let span = Span::merge(expr.span(), end_span);
                    expr = Expr::Try {
                        id,
                        span,
                        expr: Box::new(expr),
                    };
                }
                // `.` — field access, method call, or `.await`
                TokenKind::Dot => {
                    match self.peek_kind_at(1) {
                        Some(TokenKind::Await) => {
                            let _ = self.advance(); // consume `.`
                            let end_span = self.advance().span; // consume `await`
                            let id = self.alloc_id();
                            let span = Span::merge(expr.span(), end_span);
                            expr = Expr::Await {
                                id,
                                span,
                                expr: Box::new(expr),
                            };
                        }
                        Some(TokenKind::Ident) | Some(TokenKind::TypeIdent) => {
                            let _ = self.advance(); // consume `.`
                            let tok = self.advance(); // consume field/method name
                            let field = Ident {
                                name: tok.literal.unwrap_or_default(),
                                span: tok.span,
                            };
                            // Check if followed by `(` → method call (possibly with `[type_args]`)
                            let type_args = self.parse_optional_type_args();
                            if self.at(TokenKind::LParen) {
                                let _ = self.advance(); // consume `(`
                                let args = self.parse_arg_list();
                                let _ = self.expect(TokenKind::RParen);
                                let id = self.alloc_id();
                                let span = Span::merge(expr.span(), self.peek().span);
                                expr = Expr::MethodCall {
                                    id,
                                    span,
                                    receiver: Box::new(expr),
                                    method: field,
                                    type_args,
                                    args,
                                };
                            } else {
                                let id = self.alloc_id();
                                let span = Span::merge(expr.span(), field.span);
                                expr = Expr::FieldAccess {
                                    id,
                                    span,
                                    object: Box::new(expr),
                                    field,
                                };
                            }
                        }
                        _ => break,
                    }
                }
                // `(` — function call
                TokenKind::LParen => {
                    let _ = self.advance(); // consume `(`
                    let type_args = Vec::new(); // type args parsed before `(` via `[...]`
                    let args = self.parse_arg_list();
                    let end_span = self
                        .expect(TokenKind::RParen)
                        .map(|t| t.span)
                        .unwrap_or_else(|_| self.peek().span);
                    let id = self.alloc_id();
                    let span = Span::merge(expr.span(), end_span);
                    expr = Expr::Call {
                        id,
                        span,
                        callee: Box::new(expr),
                        args,
                        type_args,
                    };
                }
                // `[` — index access, or type application on a type name
                TokenKind::LBracket => {
                    // Detect `TypeName[TypeArgs].method(...)` — e.g.,
                    // `Channel[String].new()`. The brackets hold type
                    // arguments which the interpreter can discard (dispatch
                    // is by qualified name), so consume and continue the
                    // postfix loop so `.method(...)` parses normally.
                    if Self::expr_is_simple_type_name(&expr) && self.is_type_args_before_dot() {
                        let _ = self.advance(); // consume `[`
                        self.skip_newlines();
                        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                            let _ = self.parse_type_expr();
                            self.skip_newlines();
                            if self.at(TokenKind::Comma) {
                                let _ = self.advance();
                                self.skip_newlines();
                            } else {
                                break;
                            }
                        }
                        let _ = self.expect(TokenKind::RBracket);
                        continue;
                    }
                    let _ = self.advance(); // consume `[`
                    let index = self.parse_expr();
                    let end_span = self
                        .expect(TokenKind::RBracket)
                        .map(|t| t.span)
                        .unwrap_or_else(|_| self.peek().span);
                    let id = self.alloc_id();
                    let span = Span::merge(expr.span(), end_span);
                    expr = Expr::Index {
                        id,
                        span,
                        object: Box::new(expr),
                        index: Box::new(index),
                    };
                }
                // `{` after TypeIdent — record construction
                TokenKind::LBrace if self.expr_is_type_path(&expr) => {
                    let path = self.expr_to_type_path(&expr);
                    let record = self.parse_record_construct(expr.span(), path);
                    expr = record;
                }
                _ => break,
            }
        }

        expr
    }

    /// Returns `true` if `expr` is a type path suitable for record construction.
    ///
    /// Handles both simple `TypeIdent` and module-qualified paths like `Mod.Type`
    /// where all segments start with uppercase.
    ///
    /// All-uppercase names like `MAX` or `LIMIT` are treated as constants, not
    /// type paths, so that `for i in 1..=MAX { … }` does not misparse `{` as
    /// record construction.
    fn expr_is_type_path(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Identifier { name, .. } => Self::is_type_name(&name.name),
            Expr::FieldAccess { object, field, .. } => {
                Self::is_type_name(&field.name) && self.expr_is_type_path(object)
            }
            _ => false,
        }
    }

    /// Returns `true` if `name` looks like a PascalCase type name rather than
    /// an UPPER_CASE constant.  A type name starts with uppercase and either
    /// is a single character (`T`, `A`) or contains at least one lowercase
    /// letter (`Point`, `MyType`).  Multi-character all-uppercase names like
    /// `MAX` or `LIMIT` are treated as constants.
    fn is_type_name(name: &str) -> bool {
        name.starts_with(|c: char| c.is_uppercase())
            && (name.len() == 1 || name.contains(|c: char| c.is_lowercase()))
    }

    /// Returns `true` if `expr` is a single identifier whose name looks like
    /// a type (PascalCase). Used to disambiguate `Type[Args].method(...)` from
    /// plain index access.
    fn expr_is_simple_type_name(expr: &Expr) -> bool {
        matches!(expr, Expr::Identifier { name, .. } if Self::is_type_name(&name.name))
    }

    /// Peek ahead to detect `[TypeIdent (, TypeIdent)*] . Ident` pattern —
    /// i.e., a type argument list immediately followed by a method access.
    /// Used to parse `Channel[String].new()` as `Channel.new()` with type
    /// arguments consumed and discarded.
    fn is_type_args_before_dot(&self) -> bool {
        let mut offset = 1; // skip past `[`
        loop {
            while self.peek_kind_at(offset) == Some(TokenKind::Newline) {
                offset += 1;
            }
            match self.peek_kind_at(offset) {
                Some(TokenKind::TypeIdent) => offset += 1,
                _ => return false,
            }
            while self.peek_kind_at(offset) == Some(TokenKind::Newline) {
                offset += 1;
            }
            match self.peek_kind_at(offset) {
                Some(TokenKind::Comma) => {
                    offset += 1;
                }
                Some(TokenKind::RBracket) => {
                    offset += 1;
                    if self.peek_kind_at(offset) != Some(TokenKind::Dot) {
                        return false;
                    }
                    offset += 1;
                    return matches!(
                        self.peek_kind_at(offset),
                        Some(TokenKind::Ident) | Some(TokenKind::TypeIdent)
                    );
                }
                _ => return false,
            }
        }
    }

    /// Convert an expression (Identifier or FieldAccess chain) to a [`TypePath`].
    fn expr_to_type_path(&self, expr: &Expr) -> TypePath {
        match expr {
            Expr::Identifier { name, span, .. } => TypePath {
                segments: vec![name.clone()],
                span: *span,
            },
            Expr::FieldAccess {
                object,
                field,
                span,
                ..
            } => {
                let mut path = self.expr_to_type_path(object);
                path.segments.push(field.clone());
                path.span = *span;
                path
            }
            _ => TypePath {
                segments: vec![],
                span: expr.span(),
            },
        }
    }

    /// Parse `[TypeArg, ...]` optional type argument list before `(`.
    fn parse_optional_type_args(&mut self) -> Vec<TypeExpr> {
        if self.at(TokenKind::LBracket) && !self.is_index_bracket() {
            let _ = self.advance(); // consume `[`
            let mut args = Vec::new();
            self.skip_newlines();
            while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                args.push(self.parse_type_expr());
                self.skip_newlines();
                if self.at(TokenKind::Comma) {
                    let _ = self.advance();
                    self.skip_newlines();
                } else {
                    break;
                }
            }
            let _ = self.expect(TokenKind::RBracket);
            args
        } else {
            Vec::new()
        }
    }

    /// Heuristic: is the `[` an index access rather than type argument list?
    ///
    /// Returns `false` (i.e. "this is type args") when the bracket contains only
    /// comma-separated `TypeIdent` tokens and is immediately followed by `(`.
    /// Otherwise returns `true` (treat as index access).
    fn is_index_bracket(&self) -> bool {
        let mut offset = 1; // skip past `[`

        loop {
            // Skip newlines
            while self.peek_kind_at(offset) == Some(TokenKind::Newline) {
                offset += 1;
            }

            // Expect a TypeIdent (uppercase identifier)
            match self.peek_kind_at(offset) {
                Some(TokenKind::TypeIdent) => offset += 1,
                _ => return true, // not a type arg list
            }

            // Skip newlines
            while self.peek_kind_at(offset) == Some(TokenKind::Newline) {
                offset += 1;
            }

            // Expect `,` (more type args) or `]` (end of list)
            match self.peek_kind_at(offset) {
                Some(TokenKind::Comma) => offset += 1,
                Some(TokenKind::RBracket) => {
                    offset += 1;
                    // Type args when followed by `(`
                    return !matches!(self.peek_kind_at(offset), Some(TokenKind::LParen));
                }
                _ => return true,
            }
        }
    }

    /// Parse a comma-separated argument list (inside already-consumed `(`).
    fn parse_arg_list(&mut self) -> Vec<Arg> {
        let mut args = Vec::new();
        self.skip_newlines();

        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let start = self.peek().span;

            // Check for `mut` prefix on argument
            let mutable = if self.at(TokenKind::Mut) {
                let _ = self.advance();
                true
            } else {
                false
            };

            // Check for labeled argument: `label: expr`
            let (label, value) =
                if self.at(TokenKind::Ident) && self.peek_kind_at(1) == Some(TokenKind::Colon) {
                    let tok = self.advance();
                    let label = Ident {
                        name: tok.literal.unwrap_or_default(),
                        span: tok.span,
                    };
                    let _ = self.advance(); // consume `:`
                    let value = self.parse_expr();
                    (Some(label), value)
                } else {
                    let value = self.parse_expr();
                    (None, value)
                };

            let end = value.span();
            args.push(Arg {
                span: Span::merge(start, end),
                label,
                mutable,
                value,
            });

            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }

        args
    }

    /// Parse record construction: `Type { field: val, name, ..spread }`.
    fn parse_record_construct(&mut self, start: Span, path: TypePath) -> Expr {
        let _ = self.advance(); // consume `{`
        let mut fields = Vec::new();
        let mut spread = None;

        self.skip_newlines();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            // `..expr` — spread
            if self.at(TokenKind::DotDot) {
                let spread_start = self.advance().span; // consume `..`
                let expr = self.parse_expr();
                let span = Span::merge(spread_start, expr.span());
                spread = Some(Box::new(RecordSpread { span, expr }));
                self.skip_newlines();
                break;
            }

            // `name: expr` or shorthand `name`
            let field_span = self.peek().span;
            if !self.at(TokenKind::Ident) {
                break;
            }
            let tok = self.advance();
            let name = Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            };

            let value = if self.at(TokenKind::Colon) {
                let _ = self.advance(); // consume `:`
                Some(self.parse_expr())
            } else {
                None // shorthand
            };

            let field_end = value.as_ref().map(|v| v.span()).unwrap_or(field_span);
            fields.push(RecordField {
                span: Span::merge(field_span, field_end),
                name,
                value,
            });

            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
            }
        }

        let end_span = self
            .expect(TokenKind::RBrace)
            .map(|t| t.span)
            .unwrap_or(start);
        let id = self.alloc_id();
        Expr::RecordConstruct {
            id,
            span: Span::merge(start, end_span),
            path,
            fields,
            spread,
        }
    }

    /// Convert a `TypeExpr` to an `Expr` (used for the RHS of `is`).
    /// Parse a primary (atom) expression.
    fn parse_primary(&mut self) -> Expr {
        let id = self.alloc_id();
        let span = self.peek().span;

        match self.peek().kind.clone() {
            // ── Literals ──────────────────────────────────────────────────────
            TokenKind::IntLiteral => {
                let tok = self.advance();
                Expr::Literal {
                    id,
                    span,
                    lit: Literal::Int(tok.literal.unwrap_or_default()),
                }
            }
            TokenKind::FloatLiteral => {
                let tok = self.advance();
                Expr::Literal {
                    id,
                    span,
                    lit: Literal::Float(tok.literal.unwrap_or_default()),
                }
            }
            TokenKind::StringLiteral
            | TokenKind::RawStringLiteral
            | TokenKind::MultiLineStringLiteral
            | TokenKind::RawMultiLineStringLiteral => {
                let tok = self.advance();
                Expr::Literal {
                    id,
                    span,
                    lit: Literal::String(tok.literal.unwrap_or_default()),
                }
            }
            TokenKind::BoolLiteral => {
                let tok = self.advance();
                // The lexer stores no literal text for BoolLiteral; check source text.
                let value = self.source.slice(tok.span) == "true";
                Expr::Literal {
                    id,
                    span,
                    lit: Literal::Bool(value),
                }
            }
            TokenKind::CharLiteral => {
                let tok = self.advance();
                Expr::Literal {
                    id,
                    span,
                    lit: Literal::Char(tok.literal.unwrap_or_default()),
                }
            }
            // ── String interpolation ──────────────────────────────────────────
            TokenKind::StringLiteralPart | TokenKind::InterpolationStart => {
                self.parse_interpolation(id, span)
            }
            // ── Identifiers ───────────────────────────────────────────────────
            TokenKind::Ident => {
                let tok = self.advance();
                let name = Ident {
                    name: tok.literal.unwrap_or_default(),
                    span: tok.span,
                };
                Expr::Identifier {
                    id,
                    span: tok.span,
                    name,
                }
            }
            TokenKind::TypeIdent
            | TokenKind::Ok_
            | TokenKind::Err_
            | TokenKind::Some_
            | TokenKind::None_ => {
                let tok = self.advance();
                // Use display name for keyword variants (Ok_, Err_ → "Ok", "Err").
                let name = Ident {
                    name: tok.literal.unwrap_or_else(|| tok.kind.to_string()),
                    span: tok.span,
                };
                Expr::Identifier {
                    id,
                    span: tok.span,
                    name,
                }
            }
            TokenKind::SelfLower => {
                let tok = self.advance();
                let name = Ident {
                    name: "self".into(),
                    span: tok.span,
                };
                Expr::Identifier {
                    id,
                    span: tok.span,
                    name,
                }
            }
            TokenKind::SelfUpper => {
                let tok = self.advance();
                let name = Ident {
                    name: "Self".into(),
                    span: tok.span,
                };
                Expr::Identifier {
                    id,
                    span: tok.span,
                    name,
                }
            }
            // ── Placeholder `_` ───────────────────────────────────────────────
            TokenKind::Underscore => {
                let _ = self.advance();
                Expr::Placeholder { id, span }
            }
            // ── `unreachable` ─────────────────────────────────────────────────
            TokenKind::Unreachable => {
                let _ = self.advance();
                // Consume optional trailing `()` so `unreachable()` works
                if self.at(TokenKind::LParen) {
                    if let Some(next) = self.tokens.get(self.pos + 1) {
                        if next.kind == TokenKind::RParen {
                            let _ = self.advance(); // (
                            let _ = self.advance(); // )
                        }
                    }
                }
                Expr::Unreachable { id, span }
            }
            // ── `return` ──────────────────────────────────────────────────────
            TokenKind::Return => {
                let _ = self.advance();
                let value = if !self.at_stmt_terminator() {
                    Some(Box::new(self.parse_expr()))
                } else {
                    None
                };
                let end = value.as_ref().map(|v| v.span()).unwrap_or(span);
                Expr::Return {
                    id,
                    span: Span::merge(span, end),
                    value,
                }
            }
            // ── `break` ───────────────────────────────────────────────────────
            TokenKind::Break => {
                let _ = self.advance();
                let value = if !self.at_stmt_terminator() {
                    Some(Box::new(self.parse_expr()))
                } else {
                    None
                };
                let end = value.as_ref().map(|v| v.span()).unwrap_or(span);
                Expr::Break {
                    id,
                    span: Span::merge(span, end),
                    value,
                }
            }
            // ── `continue` ────────────────────────────────────────────────────
            TokenKind::Continue => {
                let _ = self.advance();
                Expr::Continue { id, span }
            }
            // ── `await expr` (prefix form) ────────────────────────────────────
            TokenKind::Await => {
                let _ = self.advance();
                let inner = self.parse_unary();
                let end = inner.span();
                Expr::Await {
                    id,
                    span: Span::merge(span, end),
                    expr: Box::new(inner),
                }
            }
            // ── `if` expression ───────────────────────────────────────────────
            TokenKind::If => self.parse_if_expr(),
            // ── `match` expression ────────────────────────────────────────────
            TokenKind::Match => self.parse_match_expr(),
            // ── `loop` expression ─────────────────────────────────────────────
            TokenKind::Loop => self.parse_loop_expr(),
            // ── Parenthesised / tuple / lambda ────────────────────────────────
            TokenKind::LParen => {
                if self.is_lambda_start() {
                    self.parse_lambda()
                } else {
                    self.parse_paren_or_tuple()
                }
            }
            // ── List literal `[...]` ──────────────────────────────────────────
            TokenKind::LBracket => self.parse_list_literal(),
            // ── Set literal `#{...}` ──────────────────────────────────────────
            TokenKind::Hash => {
                if self.peek_kind_at(1) == Some(TokenKind::LBrace) {
                    self.parse_set_literal()
                } else {
                    // Stray `#` — skip and return error expr
                    let _ = self.advance();
                    self.diagnostics.error(
                        DiagnosticCode {
                            prefix: 'E',
                            number: 2022,
                        },
                        "expected `{` after `#` for set literal".to_string(),
                        span,
                    );
                    Expr::Literal {
                        id,
                        span,
                        lit: Literal::Unit,
                    }
                }
            }
            // ── Block or map literal `{...}` ──────────────────────────────────
            TokenKind::LBrace => {
                if self.is_map_literal_start() {
                    self.parse_map_literal()
                } else {
                    let block = self.parse_block();
                    let block_span = block.span;
                    Expr::Block {
                        id: self.alloc_id(),
                        span: block_span,
                        block,
                    }
                }
            }
            _ => {
                self.diagnostics.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 2020,
                    },
                    format!("expected expression, found `{}`", self.peek().kind),
                    span,
                );
                // Skip the offending token to avoid infinite loops
                if !self.at(TokenKind::Eof) {
                    let _ = self.advance();
                }
                Expr::Literal {
                    id,
                    span,
                    lit: Literal::Unit,
                }
            }
        }
    }

    /// Parse string interpolation: sequences of `StringLiteralPart` and `${expr}`.
    fn parse_interpolation(&mut self, id: NodeId, span: Span) -> Expr {
        let mut parts = Vec::new();
        let mut end = span;

        loop {
            match self.peek().kind.clone() {
                TokenKind::StringLiteralPart => {
                    let tok = self.advance();
                    end = tok.span;
                    parts.push(InterpolationPart::Literal(tok.literal.unwrap_or_default()));
                }
                TokenKind::InterpolationStart => {
                    let _ = self.advance(); // consume `${`
                    let expr = self.parse_expr();
                    end = expr.span();
                    parts.push(InterpolationPart::Expr(expr));
                    // expect `}` (InterpolationEnd)
                    if self.at(TokenKind::InterpolationEnd) || self.at(TokenKind::RBrace) {
                        end = self.advance().span;
                    }
                }
                _ => break,
            }
        }

        Expr::Interpolation {
            id,
            span: Span::merge(span, end),
            parts,
        }
    }

    /// Lookahead: does `(...)` look like a lambda parameter list followed by `=>`?
    fn is_lambda_start(&self) -> bool {
        // We're pointing at `(`. Find the matching `)` then check for `=>`.
        let mut i = self.pos + 1; // skip `(`
        let mut depth = 1usize;

        while i < self.tokens.len() {
            match &self.tokens[i].kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                TokenKind::Eof => return false,
                _ => {}
            }
            i += 1;
        }

        // Skip newlines after `)`
        while i < self.tokens.len() && self.tokens[i].kind == TokenKind::Newline {
            i += 1;
        }

        matches!(
            self.tokens.get(i).map(|t| &t.kind),
            Some(TokenKind::FatArrow)
        )
    }

    /// Parse `(params) => body`.
    fn parse_lambda(&mut self) -> Expr {
        let start = self.peek().span;
        let _ = self.advance(); // consume `(`
        let params = self.parse_lambda_param_list();
        let _ = self.expect(TokenKind::RParen);
        let _ = self.expect(TokenKind::FatArrow);

        // Body: block or single expression
        let body = if self.at(TokenKind::LBrace) {
            let block = self.parse_block();
            let bspan = block.span;
            Expr::Block {
                id: self.alloc_id(),
                span: bspan,
                block,
            }
        } else {
            self.parse_expr()
        };

        let id = self.alloc_id();
        let span = Span::merge(start, body.span());
        Expr::Lambda {
            id,
            span,
            params,
            body: Box::new(body),
        }
    }

    /// Parse lambda params (simplified: `ident [: type]` separated by commas).
    fn parse_lambda_param_list(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        self.skip_newlines();

        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let id = self.alloc_id();
            let start = self.peek().span;

            let pattern = match self.peek().kind.clone() {
                TokenKind::Ident => {
                    let tok = self.advance();
                    let span = tok.span;
                    Pattern::Bind {
                        id: self.alloc_id(),
                        span,
                        name: Ident {
                            name: tok.literal.unwrap_or_default(),
                            span,
                        },
                    }
                }
                TokenKind::Underscore => {
                    let tok = self.advance();
                    Pattern::Wildcard {
                        id: self.alloc_id(),
                        span: tok.span,
                    }
                }
                TokenKind::Mut => {
                    let _ = self.advance(); // consume `mut`
                    let tok = if self.at(TokenKind::Ident) {
                        self.advance()
                    } else {
                        return params; // error recovery
                    };
                    let span = tok.span;
                    Pattern::MutBind {
                        id: self.alloc_id(),
                        span,
                        name: Ident {
                            name: tok.literal.unwrap_or_default(),
                            span,
                        },
                    }
                }
                _ => break,
            };

            let ty = if self.at(TokenKind::Colon) {
                let _ = self.advance();
                Some(self.parse_type_expr())
            } else {
                None
            };

            let end = self.peek().span;
            params.push(Param {
                id,
                span: Span::merge(start, end),
                pattern,
                ty,
                default: None,
            });

            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }

        params
    }

    /// Parse `(expr)` grouping or `(a, b, ...)` tuple.
    fn parse_paren_or_tuple(&mut self) -> Expr {
        let start = self.peek().span;
        let _ = self.advance(); // consume `(`

        self.skip_newlines();

        // Empty parens → unit
        if self.at(TokenKind::RParen) {
            let end = self.advance().span;
            let id = self.alloc_id();
            return Expr::Literal {
                id,
                span: Span::merge(start, end),
                lit: Literal::Unit,
            };
        }

        let first = self.parse_expr();
        self.skip_newlines();

        if self.at(TokenKind::Comma) {
            // Tuple
            let mut elems = vec![first];
            while self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
                if self.at(TokenKind::RParen) {
                    break;
                }
                elems.push(self.parse_expr());
                self.skip_newlines();
            }
            let end = self
                .expect(TokenKind::RParen)
                .map(|t| t.span)
                .unwrap_or(start);
            let id = self.alloc_id();
            Expr::TupleLiteral {
                id,
                span: Span::merge(start, end),
                elems,
            }
        } else {
            // Grouped expression
            let end = self
                .expect(TokenKind::RParen)
                .map(|t| t.span)
                .unwrap_or(start);
            let mut e = first;
            // Update span to include parens
            if let Some(new_span) = Some(Span::merge(start, end)) {
                match &mut e {
                    Expr::Literal { span, .. }
                    | Expr::Identifier { span, .. }
                    | Expr::Binary { span, .. }
                    | Expr::Unary { span, .. } => *span = new_span,
                    _ => {}
                }
            }
            e
        }
    }

    /// Parse a list literal `[elem, ...]`.
    fn parse_list_literal(&mut self) -> Expr {
        let start = self.peek().span;
        let _ = self.advance(); // consume `[`
        let mut elems = Vec::new();
        self.skip_newlines();

        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
            elems.push(self.parse_expr());
            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }

        let end = self
            .expect(TokenKind::RBracket)
            .map(|t| t.span)
            .unwrap_or(start);
        let id = self.alloc_id();
        Expr::ListLiteral {
            id,
            span: Span::merge(start, end),
            elems,
        }
    }

    /// Parse a set literal `#{elem, ...}`.
    fn parse_set_literal(&mut self) -> Expr {
        let start = self.peek().span;
        let _ = self.advance(); // consume `#`
        let _ = self.advance(); // consume `{`
        let mut elems = Vec::new();
        self.skip_newlines();

        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            elems.push(self.parse_expr());
            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }

        let end = self
            .expect(TokenKind::RBrace)
            .map(|t| t.span)
            .unwrap_or(start);
        let id = self.alloc_id();
        Expr::SetLiteral {
            id,
            span: Span::merge(start, end),
            elems,
        }
    }

    /// Check if the current position is an empty brace pair `{}`,
    /// allowing for intervening newlines.
    fn is_empty_brace(&self) -> bool {
        if self.peek().kind != TokenKind::LBrace {
            return false;
        }
        let mut i = self.pos + 1;
        while i < self.tokens.len() && self.tokens[i].kind == TokenKind::Newline {
            i += 1;
        }
        i < self.tokens.len() && self.tokens[i].kind == TokenKind::RBrace
    }

    /// Check if a type annotation refers to `Map[...]`.
    fn is_map_type_annotation(ty: &Option<TypeExpr>) -> bool {
        matches!(ty, Some(TypeExpr::Named { path, .. })
            if path.segments.last().map(|s| s.name.as_str()) == Some("Map"))
    }

    /// Lookahead: does `{` start a map literal (first element is `expr ':'`)?
    fn is_map_literal_start(&self) -> bool {
        // We're at `{`. Look at the first non-newline token after it.
        let mut i = self.pos + 1;
        while i < self.tokens.len() && self.tokens[i].kind == TokenKind::Newline {
            i += 1;
        }
        if i >= self.tokens.len() {
            return false;
        }
        // If the first token is a string/int/float literal, and next is `:`, it's a map.
        let is_map_key_start = matches!(
            &self.tokens[i].kind,
            TokenKind::StringLiteral
                | TokenKind::RawStringLiteral
                | TokenKind::RawMultiLineStringLiteral
                | TokenKind::IntLiteral
                | TokenKind::FloatLiteral
                | TokenKind::Ident
                | TokenKind::TypeIdent
        );
        if !is_map_key_start || i + 1 >= self.tokens.len() {
            return false;
        }
        self.tokens[i + 1].kind == TokenKind::Colon
    }

    /// Parse a map literal `{ key: val, ... }`.
    fn parse_map_literal(&mut self) -> Expr {
        let start = self.peek().span;
        let _ = self.advance(); // consume `{`
        let mut entries = Vec::new();
        self.skip_newlines();

        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let key = self.parse_expr();
            let _ = self.expect(TokenKind::Colon);
            let val = self.parse_expr();
            entries.push((key, val));
            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }

        let end = self
            .expect(TokenKind::RBrace)
            .map(|t| t.span)
            .unwrap_or(start);
        let id = self.alloc_id();
        Expr::MapLiteral {
            id,
            span: Span::merge(start, end),
            entries,
        }
    }

    /// Parse an `if` / `if-let` expression.
    fn parse_if_expr(&mut self) -> Expr {
        let start = self.peek().span;
        let _ = self.advance(); // consume `if`

        let _ = self.expect(TokenKind::LParen);

        // Check for `if-let`: `if (let Some(v) = expr)`
        let (let_pattern, condition) = if self.at(TokenKind::Let) {
            let _ = self.advance(); // consume `let`
            let pat = self.parse_pattern();
            let _ = self.expect(TokenKind::Assign);
            let cond = self.parse_expr();
            (Some(pat), cond)
        } else {
            (None, self.parse_expr())
        };

        let _ = self.expect(TokenKind::RParen);
        self.skip_newlines();

        let then_block = self.parse_block();

        // Continuation rule (spec §3.2 rule 8): next line starts with `else`
        // → allow `else` on a new line after closing brace.
        if self.at(TokenKind::Newline) && self.peek_past_newlines_kind() == Some(TokenKind::Else) {
            self.skip_newlines();
        }

        // Optional `else` branch
        let else_block = if self.at(TokenKind::Else) {
            let _ = self.advance(); // consume `else`
            self.skip_newlines();
            if self.at(TokenKind::If) {
                // `else if` chain
                Some(Box::new(self.parse_if_expr()))
            } else {
                let block = self.parse_block();
                let bspan = block.span;
                Some(Box::new(Expr::Block {
                    id: self.alloc_id(),
                    span: bspan,
                    block,
                }))
            }
        } else {
            None
        };

        let end = else_block
            .as_ref()
            .map(|e| e.span())
            .unwrap_or(then_block.span);
        let id = self.alloc_id();
        Expr::If {
            id,
            span: Span::merge(start, end),
            let_pattern,
            condition: Box::new(condition),
            then_block,
            else_block,
        }
    }

    /// Parse a `match` expression.
    fn parse_match_expr(&mut self) -> Expr {
        let start = self.peek().span;
        let _ = self.advance(); // consume `match`

        let scrutinee = self.parse_expr();
        self.skip_newlines();

        let _ = self.expect(TokenKind::LBrace);
        let mut arms = Vec::new();

        loop {
            self.skip_newlines();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            // Parse pattern
            let arm_start = self.peek().span;
            let pattern = self.parse_pattern();

            // Optional guard `if (condition)`
            let guard = if self.at(TokenKind::If) {
                let _ = self.advance(); // consume `if`
                let _ = self.expect(TokenKind::LParen);
                let g = self.parse_expr();
                let _ = self.expect(TokenKind::RParen);
                Some(g)
            } else {
                None
            };

            let _ = self.expect(TokenKind::FatArrow);
            self.skip_newlines();

            // Body: block or expression
            let body = if self.at(TokenKind::LBrace) {
                let block = self.parse_block();
                let bspan = block.span;
                Expr::Block {
                    id: self.alloc_id(),
                    span: bspan,
                    block,
                }
            } else {
                self.parse_expr()
            };

            let arm_end = body.span();
            arms.push(MatchArm {
                id: self.alloc_id(),
                span: Span::merge(arm_start, arm_end),
                pattern,
                guard,
                body,
            });

            self.skip_newlines();
            // Optional comma after arm
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
            }
        }

        let end = self
            .expect(TokenKind::RBrace)
            .map(|t| t.span)
            .unwrap_or(start);
        let id = self.alloc_id();
        Expr::Match {
            id,
            span: Span::merge(start, end),
            scrutinee: Box::new(scrutinee),
            arms,
        }
    }

    /// Parse a pattern (for `match`, `let`, etc.).
    pub(crate) fn parse_pattern(&mut self) -> Pattern {
        // First parse a simple pattern, then check for `|` (or-pattern)
        let first = self.parse_simple_pattern();

        if self.at(TokenKind::BitOr) {
            let start = first.span();
            let mut alternatives = vec![first];
            while self.at(TokenKind::BitOr) {
                let _ = self.advance(); // consume `|`
                self.skip_newlines();
                alternatives.push(self.parse_simple_pattern());
            }
            let end = alternatives.last().map(|p| p.span()).unwrap_or(start);
            Pattern::Or {
                id: self.alloc_id(),
                span: Span::merge(start, end),
                alternatives,
            }
        } else {
            first
        }
    }

    /// Parse a single pattern (no or-pattern).
    fn parse_simple_pattern(&mut self) -> Pattern {
        let id = self.alloc_id();
        let span = self.peek().span;

        match self.peek().kind.clone() {
            // `_` wildcard
            TokenKind::Underscore => {
                let _ = self.advance();
                Pattern::Wildcard { id, span }
            }
            // `mut name`
            TokenKind::Mut => {
                let _ = self.advance(); // consume `mut`
                let tok = if self.at(TokenKind::Ident) {
                    self.advance()
                } else {
                    return Pattern::Wildcard { id, span };
                };
                let name = Ident {
                    name: tok.literal.unwrap_or_default(),
                    span: tok.span,
                };
                Pattern::MutBind {
                    id,
                    span: Span::merge(span, tok.span),
                    name,
                }
            }
            // `..` rest pattern
            TokenKind::DotDot => {
                let _ = self.advance();
                Pattern::Rest { id, span }
            }
            // Literal patterns
            TokenKind::IntLiteral => {
                let tok = self.advance();
                let lit = Literal::Int(tok.literal.unwrap_or_default());
                let pat = Pattern::Literal {
                    id,
                    span: tok.span,
                    lit,
                };
                // Check for range pattern: `1..10` or `1..=10`
                self.try_parse_range_pattern(pat)
            }
            TokenKind::Minus => {
                // Negative literal
                let _ = self.advance();
                if self.at(TokenKind::IntLiteral) {
                    let tok = self.advance();
                    let lit = Literal::Int(format!("-{}", tok.literal.unwrap_or_default()));
                    let pat = Pattern::Literal {
                        id,
                        span: Span::merge(span, tok.span),
                        lit,
                    };
                    self.try_parse_range_pattern(pat)
                } else if self.at(TokenKind::FloatLiteral) {
                    let tok = self.advance();
                    let lit = Literal::Float(format!("-{}", tok.literal.unwrap_or_default()));
                    Pattern::Literal {
                        id,
                        span: Span::merge(span, tok.span),
                        lit,
                    }
                } else {
                    Pattern::Wildcard { id, span }
                }
            }
            TokenKind::FloatLiteral => {
                let tok = self.advance();
                Pattern::Literal {
                    id,
                    span: tok.span,
                    lit: Literal::Float(tok.literal.unwrap_or_default()),
                }
            }
            TokenKind::StringLiteral
            | TokenKind::RawStringLiteral
            | TokenKind::MultiLineStringLiteral
            | TokenKind::RawMultiLineStringLiteral => {
                let tok = self.advance();
                Pattern::Literal {
                    id,
                    span: tok.span,
                    lit: Literal::String(tok.literal.unwrap_or_default()),
                }
            }
            TokenKind::BoolLiteral => {
                let tok = self.advance();
                let val = self.source.slice(tok.span) == "true";
                Pattern::Literal {
                    id,
                    span: tok.span,
                    lit: Literal::Bool(val),
                }
            }
            // `TypeIdent` — constructor or record pattern
            TokenKind::TypeIdent
            | TokenKind::Ok_
            | TokenKind::Err_
            | TokenKind::Some_
            | TokenKind::None_ => {
                let tok = self.advance();
                // For keyword tokens (Ok, Err, Some, None), `literal` is None,
                // so fall back to the token kind's display name.
                let name = tok.literal.unwrap_or_else(|| tok.kind.to_string());
                let path_name = Ident {
                    name,
                    span: tok.span,
                };
                let path = TypePath {
                    segments: vec![path_name],
                    span: tok.span,
                };

                if self.at(TokenKind::LParen) {
                    // Constructor pattern: `Type(fields...)`
                    let _ = self.advance();
                    let mut fields = Vec::new();
                    self.skip_newlines();
                    while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                        fields.push(self.parse_pattern());
                        self.skip_newlines();
                        if self.at(TokenKind::Comma) {
                            let _ = self.advance();
                            self.skip_newlines();
                        } else {
                            break;
                        }
                    }
                    let end = self
                        .expect(TokenKind::RParen)
                        .map(|t| t.span)
                        .unwrap_or(span);
                    Pattern::Constructor {
                        id,
                        span: Span::merge(span, end),
                        path,
                        fields,
                    }
                } else if self.at(TokenKind::LBrace) {
                    // Record pattern: `Type { field, field: pat }` or `Type { field: pat, .. }`
                    let _ = self.advance();
                    let mut fields = Vec::new();
                    let mut rest = false;
                    self.skip_newlines();
                    while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                        if self.at(TokenKind::DotDot) {
                            let _ = self.advance(); // consume `..`
                            rest = true;
                            self.skip_newlines();
                            break;
                        } else if self.at(TokenKind::Ident) {
                            let ftok = self.advance();
                            let fname = Ident {
                                name: ftok.literal.unwrap_or_default(),
                                span: ftok.span,
                            };
                            let fpat = if self.at(TokenKind::Colon) {
                                let _ = self.advance();
                                Some(self.parse_pattern())
                            } else {
                                None // shorthand
                            };
                            fields.push(RecordPatternField {
                                span: fname.span,
                                name: fname,
                                pattern: fpat,
                            });
                        } else {
                            break;
                        }
                        self.skip_newlines();
                        if self.at(TokenKind::Comma) {
                            let _ = self.advance();
                            self.skip_newlines();
                        } else {
                            break;
                        }
                    }
                    let end = self
                        .expect(TokenKind::RBrace)
                        .map(|t| t.span)
                        .unwrap_or(span);
                    Pattern::Record {
                        id,
                        span: Span::merge(span, end),
                        path,
                        fields,
                        rest,
                    }
                } else {
                    // Unit constructor
                    Pattern::Constructor {
                        id,
                        span: path.span,
                        path,
                        fields: vec![],
                    }
                }
            }
            // Lowercase ident — bind pattern
            TokenKind::Ident => {
                let tok = self.advance();
                let name = Ident {
                    name: tok.literal.unwrap_or_default(),
                    span: tok.span,
                };
                Pattern::Bind {
                    id,
                    span: tok.span,
                    name,
                }
            }
            // `(a, b)` tuple pattern
            TokenKind::LParen => {
                let _ = self.advance();
                let mut elems = Vec::new();
                self.skip_newlines();
                while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                    elems.push(self.parse_pattern());
                    self.skip_newlines();
                    if self.at(TokenKind::Comma) {
                        let _ = self.advance();
                        self.skip_newlines();
                    } else {
                        break;
                    }
                }
                let end = self
                    .expect(TokenKind::RParen)
                    .map(|t| t.span)
                    .unwrap_or(span);
                if elems.len() == 1 {
                    elems.remove(0) // unwrap single-element parens
                } else {
                    Pattern::Tuple {
                        id,
                        span: Span::merge(span, end),
                        elems,
                    }
                }
            }
            // `[head, ..tail]` list pattern
            TokenKind::LBracket => {
                let _ = self.advance();
                let mut elems = Vec::new();
                let mut rest = None;
                self.skip_newlines();

                while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                    if self.at(TokenKind::DotDot) {
                        let rest_start = self.peek().span;
                        let _ = self.advance(); // consume `..`
                        if self.at(TokenKind::Ident) {
                            let tok = self.advance();
                            let name = Ident {
                                name: tok.literal.unwrap_or_default(),
                                span: tok.span,
                            };
                            rest = Some(Box::new(Pattern::Bind {
                                id: self.alloc_id(),
                                span: Span::merge(rest_start, tok.span),
                                name,
                            }));
                        } else {
                            rest = Some(Box::new(Pattern::Rest {
                                id: self.alloc_id(),
                                span: rest_start,
                            }));
                        }
                        self.skip_newlines();
                        break;
                    }
                    elems.push(self.parse_pattern());
                    self.skip_newlines();
                    if self.at(TokenKind::Comma) {
                        let _ = self.advance();
                        self.skip_newlines();
                    } else {
                        break;
                    }
                }

                let end = self
                    .expect(TokenKind::RBracket)
                    .map(|t| t.span)
                    .unwrap_or(span);
                Pattern::List {
                    id,
                    span: Span::merge(span, end),
                    elems,
                    rest,
                }
            }
            _ => {
                self.diagnostics.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 2021,
                    },
                    format!("expected pattern, found `{}`", self.peek().kind),
                    span,
                );
                if !self.at(TokenKind::Eof) {
                    let _ = self.advance();
                }
                Pattern::Wildcard { id, span }
            }
        }
    }

    /// Check if the pattern is followed by `..` or `..=` to form a range pattern.
    fn try_parse_range_pattern(&mut self, lo: Pattern) -> Pattern {
        if self.at(TokenKind::DotDot) || self.at(TokenKind::DotDotEq) {
            let inclusive = self.at(TokenKind::DotDotEq);
            let _ = self.advance();
            let hi = self.parse_simple_pattern();
            let span = Span::merge(lo.span(), hi.span());
            Pattern::Range {
                id: self.alloc_id(),
                span,
                lo: Box::new(lo),
                hi: Box::new(hi),
                inclusive,
            }
        } else {
            lo
        }
    }

    /// Returns `true` if the current token terminates a statement (newline, `;`, `}`, or EOF).
    fn at_stmt_terminator(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::RBrace | TokenKind::Eof
        )
    }

    // ─── Block parsing ────────────────────────────────────────────────────────

    /// Parse a block `{ stmts... [tail_expr] }`.
    fn parse_block(&mut self) -> Block {
        let start = self.peek().span;
        if self.expect(TokenKind::LBrace).is_err() {
            return Block {
                id: self.alloc_id(),
                span: start,
                stmts: vec![],
                tail: None,
            };
        }

        let mut stmts = Vec::new();
        let mut tail: Option<Box<Expr>> = None;

        loop {
            self.skip_newlines();
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            // Skip semicolons
            if self.at(TokenKind::Semicolon) {
                let _ = self.advance();
                continue;
            }

            let _stmt_start = self.peek().span;

            // Statements that begin with keywords
            match self.peek().kind.clone() {
                TokenKind::Let => {
                    stmts.push(Stmt::Let(self.parse_let_stmt()));
                    self.skip_newlines();
                    continue;
                }
                TokenKind::For => {
                    stmts.push(Stmt::For(self.parse_for_loop()));
                    self.skip_newlines();
                    continue;
                }
                TokenKind::While => {
                    stmts.push(Stmt::While(self.parse_while_loop()));
                    self.skip_newlines();
                    continue;
                }
                TokenKind::Loop => {
                    let expr = self.parse_loop_expr();
                    self.skip_newlines();
                    if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                        tail = Some(Box::new(expr));
                        break;
                    }
                    stmts.push(Stmt::Expr(expr));
                    continue;
                }
                TokenKind::Guard => {
                    stmts.push(Stmt::Guard(self.parse_guard_stmt()));
                    self.skip_newlines();
                    continue;
                }
                TokenKind::Handling => {
                    stmts.push(Stmt::Handling(self.parse_handling_block()));
                    self.skip_newlines();
                    continue;
                }
                _ => {}
            }

            // Parse an expression
            let expr = self.parse_expr();

            // Check if this expression is the tail (not followed by a terminator in the same line,
            // and we're about to hit `}`).
            self.skip_newlines();

            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                // This expression is the tail value of the block
                tail = Some(Box::new(expr));
                break;
            }

            // Assignment might have been parsed as assignment expr; treat as stmt
            if self.at(TokenKind::Semicolon) {
                let _ = self.advance();
            }

            stmts.push(Stmt::Expr(expr));
        }

        let fallback_end = stmts
            .last()
            .map(|s| match s {
                Stmt::Expr(e) => e.span(),
                Stmt::Let(l) => l.span,
                _ => start,
            })
            .or_else(|| tail.as_ref().map(|t| t.span()))
            .unwrap_or(start);
        let end = self
            .expect(TokenKind::RBrace)
            .map(|t| t.span)
            .unwrap_or(fallback_end);
        Block {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            stmts,
            tail,
        }
    }

    /// Parse `let [mut] pattern [: Type] = expr`.
    fn parse_let_stmt(&mut self) -> LetStmt {
        let id = self.alloc_id();
        let start = self.peek().span;
        let _ = self.advance(); // consume `let`

        let pattern = if self.at(TokenKind::Mut) {
            let mut_span = self.advance().span; // consume `mut`
            if self.at(TokenKind::Ident) {
                let tok = self.advance();
                let name = Ident {
                    name: tok.literal.unwrap_or_default(),
                    span: tok.span,
                };
                Pattern::MutBind {
                    id: self.alloc_id(),
                    span: Span::merge(mut_span, tok.span),
                    name,
                }
            } else {
                self.parse_pattern()
            }
        } else {
            self.parse_pattern()
        };

        let ty = if self.at(TokenKind::Colon) {
            let _ = self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };

        let _ = self.expect(TokenKind::Assign);
        let value = if self.is_empty_brace() && Self::is_map_type_annotation(&ty) {
            // FC-15: `let m: Map[K, V] = {}` → empty map literal, not empty block
            let open = self.advance().span; // consume `{`
            self.skip_newlines();
            let close_span = self
                .expect(TokenKind::RBrace)
                .map(|t| t.span)
                .unwrap_or(open);
            Expr::MapLiteral {
                id: self.alloc_id(),
                span: Span::merge(open, close_span),
                entries: vec![],
            }
        } else {
            self.parse_expr()
        };
        let end = value.span();

        LetStmt {
            id,
            span: Span::merge(start, end),
            pattern,
            ty,
            value,
        }
    }

    /// Parse `for pattern in expr { body }`.
    fn parse_for_loop(&mut self) -> ForLoop {
        let id = self.alloc_id();
        let start = self.peek().span;
        let _ = self.advance(); // consume `for`

        let pattern = self.parse_pattern();
        let _ = self.expect(TokenKind::In);
        let iterable = self.parse_expr();
        self.skip_newlines();
        let body = self.parse_block();

        let end = body.span;
        ForLoop {
            id,
            span: Span::merge(start, end),
            pattern,
            iterable,
            body,
        }
    }

    /// Parse `while (condition) { body }`.
    fn parse_while_loop(&mut self) -> WhileLoop {
        let id = self.alloc_id();
        let start = self.peek().span;
        let _ = self.advance(); // consume `while`

        let _ = self.expect(TokenKind::LParen);
        let condition = self.parse_expr();
        let _ = self.expect(TokenKind::RParen);
        self.skip_newlines();
        let body = self.parse_block();

        let end = body.span;
        WhileLoop {
            id,
            span: Span::merge(start, end),
            condition,
            body,
        }
    }

    /// Parse `loop { body }` as an expression.
    fn parse_loop_expr(&mut self) -> Expr {
        let id = self.alloc_id();
        let start = self.peek().span;
        let _ = self.advance(); // consume `loop`
        self.skip_newlines();
        let body = self.parse_block();
        let end = body.span;
        Expr::Loop {
            id,
            span: Span::merge(start, end),
            body,
        }
    }

    /// Parse `guard (condition) else { diverging_block }`.
    fn parse_guard_stmt(&mut self) -> GuardStmt {
        let id = self.alloc_id();
        let start = self.peek().span;
        let _ = self.advance(); // consume `guard`
        let _ = self.expect(TokenKind::LParen);

        // Check for `guard (let pat = expr)` — same condition production as `if`.
        let (let_pattern, condition) = if self.at(TokenKind::Let) {
            let _ = self.advance(); // consume `let`
            let pat = self.parse_pattern();
            let _ = self.expect(TokenKind::Assign);
            let cond = self.parse_expr();
            (Some(pat), cond)
        } else {
            (None, self.parse_expr())
        };

        let _ = self.expect(TokenKind::RParen);
        let _ = self.expect(TokenKind::Else);
        self.skip_newlines();
        let else_block = self.parse_block();
        let end = else_block.span;
        GuardStmt {
            id,
            span: Span::merge(start, end),
            let_pattern,
            condition,
            else_block,
        }
    }

    /// Parse `handling (Effect with handler, ...) { body }`.
    fn parse_handling_block(&mut self) -> HandlingBlock {
        let id = self.alloc_id();
        let start = self.peek().span;
        let _ = self.advance(); // consume `handling`
        let _ = self.expect(TokenKind::LParen);

        let mut handlers = Vec::new();
        self.skip_newlines();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let h_start = self.peek().span;
            let effect = self.parse_type_path();
            let _ = self.expect(TokenKind::With);
            let handler = self.parse_expr();
            let h_end = handler.span();
            handlers.push(HandlerPair {
                span: Span::merge(h_start, h_end),
                effect,
                handler,
            });
            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }
        let _ = self.expect(TokenKind::RParen);
        self.skip_newlines();
        let body = self.parse_block();
        let end = body.span;
        HandlingBlock {
            id,
            span: Span::merge(start, end),
            handlers,
            body,
        }
    }

    // ─── Function declarations ────────────────────────────────────────────────

    /// Parse a function declaration.
    ///
    /// ```text
    /// [vis] [async] fn IDENT [generic_params] ( [params] ) [-> type] [with effects] [where] block
    /// ```
    fn parse_fn_decl(&mut self, annotations: Vec<Annotation>, vis: Visibility) -> FnDecl {
        let start = self.peek().span;

        // Optional `async` keyword.
        let is_async = if self.at(TokenKind::Async) {
            let _ = self.advance();
            true
        } else {
            false
        };

        let _ = self.expect(TokenKind::Fn); // consume `fn`

        // Function name — must be a lowercase identifier.
        let name_span = self.peek().span;
        let name = if self.at(TokenKind::Ident) {
            let tok = self.advance();
            Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            }
        } else {
            self.diagnostics.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 2030,
                },
                format!("expected function name, found `{}`", self.peek().kind),
                name_span,
            );
            Ident {
                name: String::new(),
                span: name_span,
            }
        };

        // Generic params `[T, U: Bound]`.
        let generic_params = self.parse_generic_params();

        // Parameter list `(x: Int, y: Int = 0)`.
        let _ = self.expect(TokenKind::LParen);
        let params = self.parse_param_list();
        let _ = self.expect(TokenKind::RParen);

        // Return type `-> Type`.
        let return_type = if self.at(TokenKind::ThinArrow) {
            let _ = self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };

        // Effect clause `with Effect1, Effect2` — may appear on the next line.
        if self.peek_past_newlines_kind() == Some(TokenKind::With) {
            self.skip_newlines();
        }
        let effect_clause = self.parse_effect_clause();

        // Where clause `where (T: Bound)` — may appear on the next line.
        if self.peek_past_newlines_kind() == Some(TokenKind::Where) {
            self.skip_newlines();
        }
        let where_clause = self.parse_where_clause();

        self.skip_newlines();

        // Body block.
        let body = self.parse_block();
        let end = body.span;

        FnDecl {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            annotations,
            visibility: vis,
            is_async,
            name,
            generic_params,
            params,
            return_type,
            effect_clause,
            where_clause,
            body: Some(body),
        }
    }

    /// Parse a comma-separated parameter list (inside the `(...)` already open).
    fn parse_param_list(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        self.skip_newlines();

        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            params.push(self.parse_param());
            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
            } else {
                break;
            }
        }

        params
    }

    /// Parse a single function parameter: `['mut'] ( 'self' | IDENT ':' type_expr ) [= default]`.
    fn parse_param(&mut self) -> Param {
        let id = self.alloc_id();
        let start = self.peek().span;

        // Pattern: optional `mut` prefix, then simple ident, `_`, or `self`.
        let pattern = match self.peek().kind.clone() {
            TokenKind::Mut => {
                let _ = self.advance(); // consume `mut`
                match self.peek().kind.clone() {
                    TokenKind::Ident => {
                        let tok = self.advance();
                        let span = tok.span;
                        Pattern::MutBind {
                            id: self.alloc_id(),
                            span,
                            name: Ident {
                                name: tok.literal.unwrap_or_default(),
                                span,
                            },
                        }
                    }
                    TokenKind::SelfLower => {
                        let tok = self.advance();
                        Pattern::MutBind {
                            id: self.alloc_id(),
                            span: tok.span,
                            name: Ident {
                                name: "self".into(),
                                span: tok.span,
                            },
                        }
                    }
                    _ => {
                        self.diagnostics.error(
                            DiagnosticCode {
                                prefix: 'E',
                                number: 2031,
                            },
                            format!(
                                "expected parameter name after `mut`, found `{}`",
                                self.peek().kind
                            ),
                            start,
                        );
                        Pattern::Wildcard {
                            id: self.alloc_id(),
                            span: start,
                        }
                    }
                }
            }
            TokenKind::Ident => {
                let tok = self.advance();
                let span = tok.span;
                Pattern::Bind {
                    id: self.alloc_id(),
                    span,
                    name: Ident {
                        name: tok.literal.unwrap_or_default(),
                        span,
                    },
                }
            }
            TokenKind::SelfLower => {
                let tok = self.advance();
                Pattern::Bind {
                    id: self.alloc_id(),
                    span: tok.span,
                    name: Ident {
                        name: "self".into(),
                        span: tok.span,
                    },
                }
            }
            TokenKind::Underscore => {
                let tok = self.advance();
                Pattern::Wildcard {
                    id: self.alloc_id(),
                    span: tok.span,
                }
            }
            _ => {
                self.diagnostics.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 2031,
                    },
                    format!("expected parameter name, found `{}`", self.peek().kind),
                    start,
                );
                Pattern::Wildcard {
                    id: self.alloc_id(),
                    span: start,
                }
            }
        };

        // Optional type annotation `: Type`.
        let ty = if self.at(TokenKind::Colon) {
            let _ = self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };

        // Optional default value `= expr`.
        let default = if self.at(TokenKind::Assign) {
            let _ = self.advance();
            Some(self.parse_expr_stub())
        } else {
            None
        };

        let end = self.peek().span;
        Param {
            id,
            span: Span::merge(start, end),
            pattern,
            ty,
            default,
        }
    }

    /// Parse `with Effect1, Effect2, ...`.
    fn parse_effect_clause(&mut self) -> Vec<TypePath> {
        if !self.at(TokenKind::With) {
            return Vec::new();
        }
        let _ = self.advance(); // consume `with`

        let mut effects = Vec::new();
        effects.push(self.parse_type_path());

        while self.at(TokenKind::Comma) {
            let _ = self.advance();
            self.skip_newlines();
            effects.push(self.parse_type_path());
        }

        effects
    }

    // ─── Type alias declarations ─────────────────────────────────────────────

    /// Parse a `type Name[T] = Type where (predicate)` alias declaration.
    fn parse_type_alias_decl(
        &mut self,
        annotations: Vec<Annotation>,
        vis: Visibility,
    ) -> TypeAliasDecl {
        let start = self.peek().span;
        let _ = self.advance(); // consume `type`

        // Name — must be a type identifier (uppercase).
        let name_span = self.peek().span;
        let name = if self.at(TokenKind::TypeIdent) {
            let tok = self.advance();
            Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            }
        } else if self.at(TokenKind::Ident) {
            // Accept lowercase identifiers with a warning-free path for now.
            let tok = self.advance();
            Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            }
        } else {
            self.diagnostics.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 2060,
                },
                format!("expected type alias name, found `{}`", self.peek().kind),
                name_span,
            );
            Ident {
                name: String::new(),
                span: name_span,
            }
        };

        // Optional generic params `[T, U]`.
        let generic_params = self.parse_generic_params();

        // `=`
        let _ = self.expect(TokenKind::Assign);

        // Type expression.
        let ty = self.parse_type_expr();

        // Optional where clause `where (predicate)`.
        let where_clause = self.parse_where_clause();

        let end = if !where_clause.is_empty() {
            where_clause
                .last()
                .expect("where_clause confirmed non-empty")
                .span
        } else {
            ty.span()
        };

        TypeAliasDecl {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            annotations,
            visibility: vis,
            name,
            generic_params,
            ty,
            where_clause,
        }
    }

    // ─── Const declarations ──────────────────────────────────────────────────

    /// Parse a `const NAME: Type = value` declaration.
    fn parse_const_decl(&mut self, annotations: Vec<Annotation>, vis: Visibility) -> ConstDecl {
        let start = self.peek().span;
        let _ = self.advance(); // consume `const`

        // Name — accept both Ident and TypeIdent (constants are UPPER_CASE by convention).
        let name_span = self.peek().span;
        let name = if matches!(self.peek().kind, TokenKind::Ident | TokenKind::TypeIdent) {
            let tok = self.advance();
            Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            }
        } else {
            self.diagnostics.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 2061,
                },
                format!("expected constant name, found `{}`", self.peek().kind),
                name_span,
            );
            Ident {
                name: String::new(),
                span: name_span,
            }
        };

        // `:`
        let _ = self.expect(TokenKind::Colon);

        // Type expression.
        let ty = self.parse_type_expr();

        // `=`
        let _ = self.expect(TokenKind::Assign);

        // Value expression.
        let value = self.parse_expr();

        let end = value.span();

        ConstDecl {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            annotations,
            visibility: vis,
            name,
            ty,
            value,
        }
    }

    // ─── Record declarations ──────────────────────────────────────────────────

    /// Parse a record (value-type) declaration.
    ///
    /// ```text
    /// [vis] record TypeIdent [generic_params] [where] { fields }
    /// ```
    fn parse_record_decl(&mut self, annotations: Vec<Annotation>, vis: Visibility) -> RecordDecl {
        let start = self.peek().span;
        let _ = self.advance(); // consume `record`

        let name_span = self.peek().span;
        let name = if self.at(TokenKind::TypeIdent) {
            let tok = self.advance();
            Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            }
        } else {
            self.diagnostics.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 2040,
                },
                format!("expected record name, found `{}`", self.peek().kind),
                name_span,
            );
            Ident {
                name: String::new(),
                span: name_span,
            }
        };

        let generic_params = self.parse_generic_params();
        let where_clause = self.parse_where_clause();

        self.skip_newlines();
        let _ = self.expect(TokenKind::LBrace);
        let fields = self.parse_record_fields();
        let end = self
            .expect(TokenKind::RBrace)
            .map(|t| t.span)
            .unwrap_or(start);

        RecordDecl {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            annotations,
            visibility: vis,
            name,
            generic_params,
            where_clause,
            fields,
        }
    }

    /// Parse fields inside a record body `{ name: Type [= default], ... }`.
    fn parse_record_fields(&mut self) -> Vec<RecordDeclField> {
        let mut fields = Vec::new();

        loop {
            self.skip_newlines();
            // Skip doc comments attached to the next field; they're consumed
            // for doc-generation tooling but not stored on the AST.
            while self.at(TokenKind::DocComment) {
                let _ = self.advance();
                self.skip_newlines();
            }
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            if !self.at(TokenKind::Ident) {
                break;
            }

            let id = self.alloc_id();
            let start = self.peek().span;
            let tok = self.advance();
            let name = Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            };

            let _ = self.expect(TokenKind::Colon);
            let ty = self.parse_type_expr();

            let default = if self.at(TokenKind::Assign) {
                let _ = self.advance();
                Some(self.parse_expr_stub())
            } else {
                None
            };

            let end = self.peek().span;
            fields.push(RecordDeclField {
                id,
                span: Span::merge(start, end),
                name,
                ty,
                default,
            });

            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
            }
        }

        fields
    }

    // ─── Enum declarations ────────────────────────────────────────────────────

    /// Parse an enum (ADT) declaration.
    ///
    /// ```text
    /// [vis] enum TypeIdent [generic_params] [where] { variants }
    /// ```
    fn parse_enum_decl(&mut self, annotations: Vec<Annotation>, vis: Visibility) -> EnumDecl {
        let start = self.peek().span;
        let _ = self.advance(); // consume `enum`

        let name_span = self.peek().span;
        let name = if self.at(TokenKind::TypeIdent) {
            let tok = self.advance();
            Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            }
        } else {
            self.diagnostics.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 2050,
                },
                format!("expected enum name, found `{}`", self.peek().kind),
                name_span,
            );
            Ident {
                name: String::new(),
                span: name_span,
            }
        };

        let generic_params = self.parse_generic_params();
        let where_clause = self.parse_where_clause();

        self.skip_newlines();
        let _ = self.expect(TokenKind::LBrace);
        let variants = self.parse_enum_variants();
        let end = self
            .expect(TokenKind::RBrace)
            .map(|t| t.span)
            .unwrap_or(start);

        EnumDecl {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            annotations,
            visibility: vis,
            name,
            generic_params,
            where_clause,
            variants,
        }
    }

    /// Parse enum variants inside `{ ... }`.
    fn parse_enum_variants(&mut self) -> Vec<EnumVariant> {
        let mut variants = Vec::new();

        loop {
            self.skip_newlines();
            while self.at(TokenKind::DocComment) {
                let _ = self.advance();
                self.skip_newlines();
            }
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }
            // Accept TypeIdent and standard-library keyword variants (Ok, Err, Some, None).
            if !matches!(
                self.peek().kind,
                TokenKind::TypeIdent
                    | TokenKind::Ok_
                    | TokenKind::Err_
                    | TokenKind::Some_
                    | TokenKind::None_
            ) {
                break;
            }

            let id = self.alloc_id();
            let start = self.peek().span;
            let tok = self.advance();
            // Use the display name for keyword variants (Ok_, Err_ → "Ok", "Err").
            let variant_name = tok.literal.unwrap_or_else(|| tok.kind.to_string());
            let name = Ident {
                name: variant_name,
                span: tok.span,
            };

            let variant = if self.at(TokenKind::LBrace) {
                // Struct variant: `Name { field: Type, ... }`.
                let _ = self.advance();
                let fields = self.parse_record_fields();
                let end = self
                    .expect(TokenKind::RBrace)
                    .map(|t| t.span)
                    .unwrap_or(start);
                EnumVariant::Struct {
                    id,
                    span: Span::merge(start, end),
                    name,
                    fields,
                }
            } else if self.at(TokenKind::LParen) {
                // Tuple variant: `Name(Type, Type)`.
                let _ = self.advance();
                let mut tys = Vec::new();
                self.skip_newlines();
                while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                    tys.push(self.parse_type_expr());
                    self.skip_newlines();
                    if self.at(TokenKind::Comma) {
                        let _ = self.advance();
                        self.skip_newlines();
                    } else {
                        break;
                    }
                }
                let end = self
                    .expect(TokenKind::RParen)
                    .map(|t| t.span)
                    .unwrap_or(start);
                EnumVariant::Tuple {
                    id,
                    span: Span::merge(start, end),
                    name,
                    tys,
                }
            } else {
                // Unit variant: just `Name`.
                EnumVariant::Unit {
                    id,
                    span: start,
                    name,
                }
            };

            variants.push(variant);

            self.skip_newlines();
            if self.at(TokenKind::Comma) {
                let _ = self.advance();
            }
        }

        variants
    }

    // ─── Class declarations ───────────────────────────────────────────────────

    /// Parse a class declaration.
    ///
    /// ```text
    /// [vis] class TypeIdent [generic_params] [: Parent {, Trait}] [where] { fields methods }
    /// ```
    fn parse_class_decl(&mut self, annotations: Vec<Annotation>, vis: Visibility) -> ClassDecl {
        let start = self.peek().span;
        let _ = self.advance(); // consume `class`

        let name_span = self.peek().span;
        let name = if self.at(TokenKind::TypeIdent) {
            let tok = self.advance();
            Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            }
        } else {
            self.diagnostics.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 2060,
                },
                format!("expected class name, found `{}`", self.peek().kind),
                name_span,
            );
            Ident {
                name: String::new(),
                span: name_span,
            }
        };

        let generic_params = self.parse_generic_params();

        // Optional `: Parent, Trait1, Trait2` inheritance/trait list.
        let mut base: Option<TypePath> = None;
        let mut traits: Vec<TypePath> = Vec::new();

        if self.at(TokenKind::Colon) {
            let _ = self.advance(); // consume `:`
                                    // First entry may be a base class or a trait — we treat it as base.
            let first = self.parse_type_path();
            base = Some(first);

            // Remaining comma-separated entries are traits.
            while self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
                traits.push(self.parse_type_path());
            }
        }

        let where_clause = self.parse_where_clause();

        self.skip_newlines();
        let _ = self.expect(TokenKind::LBrace);

        let (fields, methods) = self.parse_class_members();

        let end = self
            .expect(TokenKind::RBrace)
            .map(|t| t.span)
            .unwrap_or(start);

        ClassDecl {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            annotations,
            visibility: vis,
            name,
            generic_params,
            base,
            traits,
            where_clause,
            fields,
            methods,
        }
    }

    /// Parse the interior of a class body: a mix of field declarations and method declarations.
    fn parse_class_members(&mut self) -> (Vec<RecordDeclField>, Vec<FnDecl>) {
        let mut fields = Vec::new();
        let mut methods = Vec::new();

        loop {
            self.skip_newlines();
            while self.at(TokenKind::DocComment) {
                let _ = self.advance();
                self.skip_newlines();
            }
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }

            // Leading annotations.
            let mut annotations = Vec::new();
            while self.at(TokenKind::At) {
                annotations.push(self.parse_annotation());
                self.skip_newlines();
            }

            // Optional visibility.
            let vis = if self.at_visibility() {
                self.parse_visibility()
            } else {
                Visibility::Private
            };

            match self.peek().kind.clone() {
                TokenKind::Fn | TokenKind::Async => {
                    methods.push(self.parse_fn_decl(annotations, vis));
                }
                TokenKind::Ident => {
                    // Field declaration: `name: Type [= default]`
                    let id = self.alloc_id();
                    let field_start = self.peek().span;
                    let tok = self.advance();
                    let field_name = Ident {
                        name: tok.literal.unwrap_or_default(),
                        span: tok.span,
                    };
                    let _ = self.expect(TokenKind::Colon);
                    let ty = self.parse_type_expr();
                    let default = if self.at(TokenKind::Assign) {
                        let _ = self.advance();
                        Some(self.parse_expr_stub())
                    } else {
                        None
                    };
                    let field_end = self.peek().span;
                    fields.push(RecordDeclField {
                        id,
                        span: Span::merge(field_start, field_end),
                        name: field_name,
                        ty,
                        default,
                    });
                    self.skip_newlines();
                    if self.at(TokenKind::Comma) {
                        let _ = self.advance();
                    }
                }
                _ => {
                    // Unrecognised — skip to avoid infinite loop.
                    let _ = self.advance();
                }
            }
        }

        (fields, methods)
    }

    // ─── Trait declarations ───────────────────────────────────────────────────

    /// Parse a trait declaration (or platform trait when `is_platform` is true).
    ///
    /// ```text
    /// [vis] trait TypeIdent [generic_params] [where] { members }
    /// ```
    fn parse_trait_decl(
        &mut self,
        annotations: Vec<Annotation>,
        vis: Visibility,
        is_platform: bool,
    ) -> TraitDecl {
        let start = self.peek().span;
        let _ = self.expect(TokenKind::Trait); // consume `trait`

        let name_span = self.peek().span;
        let name = if self.at(TokenKind::TypeIdent) {
            let tok = self.advance();
            Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            }
        } else {
            self.diagnostics.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 2070,
                },
                format!("expected trait name, found `{}`", self.peek().kind),
                name_span,
            );
            Ident {
                name: String::new(),
                span: name_span,
            }
        };

        let generic_params = self.parse_generic_params();

        // Optional supertrait list `: Supertrait1, Supertrait2`.
        let mut supertraits = Vec::new();
        if self.at(TokenKind::Colon) {
            let _ = self.advance();
            supertraits.push(self.parse_type_path());
            while self.at(TokenKind::Comma) {
                let _ = self.advance();
                self.skip_newlines();
                supertraits.push(self.parse_type_path());
            }
        }

        let _where_clause = self.parse_where_clause();

        self.skip_newlines();
        let _ = self.expect(TokenKind::LBrace);

        let (associated_types, methods) = self.parse_trait_members();

        let end = self
            .expect(TokenKind::RBrace)
            .map(|t| t.span)
            .unwrap_or(start);

        TraitDecl {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            annotations,
            visibility: vis,
            is_platform,
            name,
            generic_params,
            supertraits,
            associated_types,
            methods,
        }
    }

    /// Parse the body of a `platform trait` — delegates to `parse_trait_decl` with the flag set.
    fn parse_platform_trait_decl(
        &mut self,
        annotations: Vec<Annotation>,
        vis: Visibility,
    ) -> TraitDecl {
        let _ = self.advance(); // consume `platform`
        self.parse_trait_decl(annotations, vis, true)
    }

    /// Parse trait body members: associated type declarations and method declarations.
    ///
    /// Methods may be required (no body) or have a default implementation (with body).
    fn parse_trait_members(&mut self) -> (Vec<AssociatedType>, Vec<FnDecl>) {
        let mut assoc_types = Vec::new();
        let mut methods = Vec::new();

        loop {
            self.skip_newlines();
            while self.at(TokenKind::DocComment) {
                let _ = self.advance();
                self.skip_newlines();
            }
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }

            // Leading annotations.
            let mut annotations = Vec::new();
            while self.at(TokenKind::At) {
                annotations.push(self.parse_annotation());
                self.skip_newlines();
            }

            // Optional visibility.
            let vis = if self.at_visibility() {
                self.parse_visibility()
            } else {
                Visibility::Private
            };

            match self.peek().kind.clone() {
                // `type AssocName [: Bound]`
                TokenKind::Type => {
                    let at_start = self.peek().span;
                    let _ = self.advance(); // consume `type`
                    let at_id = self.alloc_id();
                    let at_name_span = self.peek().span;
                    let at_name = if self.at(TokenKind::TypeIdent) {
                        let tok = self.advance();
                        Ident {
                            name: tok.literal.unwrap_or_default(),
                            span: tok.span,
                        }
                    } else {
                        self.diagnostics.error(
                            DiagnosticCode {
                                prefix: 'E',
                                number: 2071,
                            },
                            format!(
                                "expected associated type name, found `{}`",
                                self.peek().kind
                            ),
                            at_name_span,
                        );
                        Ident {
                            name: String::new(),
                            span: at_name_span,
                        }
                    };

                    let bounds = if self.at(TokenKind::Colon) {
                        let _ = self.advance();
                        vec![self.parse_type_path()]
                    } else {
                        Vec::new()
                    };

                    let at_end = self.peek().span;
                    assoc_types.push(AssociatedType {
                        id: at_id,
                        span: Span::merge(at_start, at_end),
                        name: at_name,
                        bounds,
                    });
                }
                TokenKind::Fn | TokenKind::Async => {
                    let fn_start = self.peek().span;
                    let is_async = if self.at(TokenKind::Async) {
                        let _ = self.advance();
                        true
                    } else {
                        false
                    };
                    let _ = self.expect(TokenKind::Fn);

                    let fn_name_span = self.peek().span;
                    let fn_name = if self.at(TokenKind::Ident) {
                        let tok = self.advance();
                        Ident {
                            name: tok.literal.unwrap_or_default(),
                            span: tok.span,
                        }
                    } else {
                        self.diagnostics.error(
                            DiagnosticCode {
                                prefix: 'E',
                                number: 2072,
                            },
                            format!("expected method name, found `{}`", self.peek().kind),
                            fn_name_span,
                        );
                        Ident {
                            name: String::new(),
                            span: fn_name_span,
                        }
                    };

                    let generic_params = self.parse_generic_params();
                    let _ = self.expect(TokenKind::LParen);
                    let params = self.parse_param_list();
                    let _ = self.expect(TokenKind::RParen);

                    let return_type = if self.at(TokenKind::ThinArrow) {
                        let _ = self.advance();
                        Some(self.parse_type_expr())
                    } else {
                        None
                    };

                    let effect_clause = self.parse_effect_clause();
                    let where_clause = self.parse_where_clause();

                    self.skip_newlines();

                    // Required methods have no body; default impls do.
                    let body = if self.at(TokenKind::LBrace) {
                        Some(self.parse_block())
                    } else {
                        None
                    };

                    let fn_end = body
                        .as_ref()
                        .map(|b| b.span)
                        .unwrap_or_else(|| self.peek().span);
                    methods.push(FnDecl {
                        id: self.alloc_id(),
                        span: Span::merge(fn_start, fn_end),
                        annotations,
                        visibility: vis,
                        is_async,
                        name: fn_name,
                        generic_params,
                        params,
                        return_type,
                        effect_clause,
                        where_clause,
                        body,
                    });
                }
                _ => {
                    // Unrecognised — skip.
                    let _ = self.advance();
                }
            }
        }

        (assoc_types, methods)
    }

    // ─── Impl blocks ──────────────────────────────────────────────────────────

    /// Parse an `impl` block.
    ///
    /// ```text
    /// impl [generic_params] [TraitPath for] TypeExpr [where] { methods }
    /// ```
    fn parse_impl_block(&mut self, annotations: Vec<Annotation>) -> ImplBlock {
        let start = self.peek().span;
        let _ = self.advance(); // consume `impl`

        let generic_params = self.parse_generic_params();

        // Determine whether this is `impl Trait for Type` or `impl Type`.
        // Disambiguate by scanning ahead: if we see `for` after a type path, it's a trait impl.
        let (trait_path, target) = self.parse_impl_header();

        let where_clause = self.parse_where_clause();

        self.skip_newlines();
        let _ = self.expect(TokenKind::LBrace);
        let methods = self.parse_impl_methods();
        let end = self
            .expect(TokenKind::RBrace)
            .map(|t| t.span)
            .unwrap_or(start);

        ImplBlock {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            annotations,
            generic_params,
            trait_path,
            target,
            where_clause,
            type_assignments: vec![],
            methods,
        }
    }

    /// Parse the `[Trait for] Type` header of an impl block.
    fn parse_impl_header(&mut self) -> (Option<TypePath>, TypeExpr) {
        // Parse the first type path and check if `for` follows.
        let first = self.parse_type_expr();

        if self.at(TokenKind::For) {
            // `impl Trait for Type`
            let _ = self.advance(); // consume `for`
            let target = self.parse_type_expr();
            // Extract the type path from the first TypeExpr (must be Named).
            let trait_path = match &first {
                TypeExpr::Named { path, .. } => Some(path.clone()),
                _ => None,
            };
            (trait_path, target)
        } else {
            // `impl Type`
            (None, first)
        }
    }

    /// Parse the methods inside an impl block body.
    fn parse_impl_methods(&mut self) -> Vec<FnDecl> {
        let mut methods = Vec::new();

        loop {
            self.skip_newlines();
            while self.at(TokenKind::DocComment) {
                let _ = self.advance();
                self.skip_newlines();
            }
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }

            // Leading annotations.
            let mut annotations = Vec::new();
            while self.at(TokenKind::At) {
                annotations.push(self.parse_annotation());
                self.skip_newlines();
            }

            // Optional visibility.
            let vis = if self.at_visibility() {
                self.parse_visibility()
            } else {
                Visibility::Private
            };

            if matches!(self.peek().kind, TokenKind::Fn | TokenKind::Async) {
                methods.push(self.parse_fn_decl(annotations, vis));
            } else {
                // Unrecognised — skip.
                if self.at(TokenKind::Eof) {
                    break;
                }
                let _ = self.advance();
            }
        }

        methods
    }

    // ─── Effect declarations ──────────────────────────────────────────────────

    /// Parse an effect declaration.
    ///
    /// ```text
    /// [vis] effect TypeIdent [generic_params] { fn_sig* }
    /// [vis] effect TypeIdent = TypePath + TypePath + ...   (composite)
    /// ```
    fn parse_effect_decl(&mut self, annotations: Vec<Annotation>, vis: Visibility) -> EffectDecl {
        let start = self.peek().span;
        let _ = self.advance(); // consume `effect`

        let name_span = self.peek().span;
        let name = if self.at(TokenKind::TypeIdent) {
            let tok = self.advance();
            Ident {
                name: tok.literal.unwrap_or_default(),
                span: tok.span,
            }
        } else {
            self.diagnostics.error(
                DiagnosticCode {
                    prefix: 'E',
                    number: 2090,
                },
                format!("expected effect name, found `{}`", self.peek().kind),
                name_span,
            );
            Ident {
                name: String::new(),
                span: name_span,
            }
        };

        let generic_params = self.parse_generic_params();

        // Composite effect: `effect Name = TypePath + TypePath + ...`
        if self.at(TokenKind::Assign) {
            let _ = self.advance(); // consume `=`
            let mut components = vec![self.parse_type_path()];
            while self.at(TokenKind::Plus) {
                let _ = self.advance();
                self.skip_newlines();
                components.push(self.parse_type_path());
            }
            let end = self.peek().span;
            // Skip optional newline at end.
            if self.at(TokenKind::Newline) {
                let _ = self.advance();
            }
            return EffectDecl {
                id: self.alloc_id(),
                span: Span::merge(start, end),
                annotations,
                visibility: vis,
                name,
                generic_params,
                components,
                operations: Vec::new(),
            };
        }

        // Regular effect with operation signatures.
        self.skip_newlines();
        let _ = self.expect(TokenKind::LBrace);
        let operations = self.parse_effect_operations();
        let end = self
            .expect(TokenKind::RBrace)
            .map(|t| t.span)
            .unwrap_or(start);

        EffectDecl {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            annotations,
            visibility: vis,
            name,
            generic_params,
            components: vec![],
            operations,
        }
    }

    /// Parse the operation signatures inside an effect body.
    ///
    /// Each operation is a `fn` signature without a body.
    fn parse_effect_operations(&mut self) -> Vec<FnDecl> {
        let mut ops = Vec::new();

        loop {
            self.skip_newlines();
            while self.at(TokenKind::DocComment) {
                let _ = self.advance();
                self.skip_newlines();
            }
            if self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                break;
            }

            // Leading annotations.
            let mut annotations = Vec::new();
            while self.at(TokenKind::At) {
                annotations.push(self.parse_annotation());
                self.skip_newlines();
            }

            let vis = if self.at_visibility() {
                self.parse_visibility()
            } else {
                Visibility::Private
            };

            if !matches!(self.peek().kind, TokenKind::Fn | TokenKind::Async) {
                if self.at(TokenKind::Eof) {
                    break;
                }
                let _ = self.advance();
                continue;
            }

            let fn_start = self.peek().span;
            let is_async = if self.at(TokenKind::Async) {
                let _ = self.advance();
                true
            } else {
                false
            };
            let _ = self.expect(TokenKind::Fn);

            let fn_name_span = self.peek().span;
            let fn_name = if self.at(TokenKind::Ident) {
                let tok = self.advance();
                Ident {
                    name: tok.literal.unwrap_or_default(),
                    span: tok.span,
                }
            } else {
                self.diagnostics.error(
                    DiagnosticCode {
                        prefix: 'E',
                        number: 2091,
                    },
                    format!("expected operation name, found `{}`", self.peek().kind),
                    fn_name_span,
                );
                Ident {
                    name: String::new(),
                    span: fn_name_span,
                }
            };

            let generic_params = self.parse_generic_params();
            let _ = self.expect(TokenKind::LParen);
            let params = self.parse_param_list();
            let _ = self.expect(TokenKind::RParen);

            let return_type = if self.at(TokenKind::ThinArrow) {
                let _ = self.advance();
                Some(self.parse_type_expr())
            } else {
                None
            };

            let effect_clause = self.parse_effect_clause();
            let where_clause = self.parse_where_clause();

            // Operations have no body.
            let fn_end = self.peek().span;

            ops.push(FnDecl {
                id: self.alloc_id(),
                span: Span::merge(fn_start, fn_end),
                annotations,
                visibility: vis,
                is_async,
                name: fn_name,
                generic_params,
                params,
                return_type,
                effect_clause,
                where_clause,
                body: None,
            });
        }

        ops
    }

    // ─── Module handle declarations ───────────────────────────────────────────

    /// Parse a module-level `handle TypePath with expr NEWLINE`.
    fn parse_module_handle_decl(&mut self) -> ModuleHandleDecl {
        let start = self.peek().span;
        let _ = self.advance(); // consume `handle`

        let effect = self.parse_type_path();
        let _ = self.expect(TokenKind::With);
        let handler = self.parse_expr();
        let end = handler.span();

        if self.at(TokenKind::Newline) {
            let _ = self.advance();
        }

        ModuleHandleDecl {
            id: self.alloc_id(),
            span: Span::merge(start, end),
            effect,
            handler,
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_errors::FileId;
    use bock_lexer::Lexer;
    use std::path::PathBuf;

    fn parse(src: &str) -> (Module, DiagnosticBag) {
        let file_id = FileId(0);
        let source = SourceFile::new(file_id, PathBuf::from("test.bock"), src.to_string());
        let tokens = Lexer::new(&source).tokenize();
        let mut parser = Parser::new(tokens, &source);
        let module = parser.parse_module();
        let diags = std::mem::replace(&mut parser.diagnostics, DiagnosticBag::new());
        (module, diags)
    }

    #[test]
    fn parse_empty_file() {
        let (m, diags) = parse("");
        assert!(m.path.is_none());
        assert!(m.imports.is_empty());
        assert!(m.items.is_empty());
        assert!(!diags.has_errors());
    }

    #[test]
    fn parse_module_declaration() {
        let (m, diags) = parse("module app.auth\n");
        assert!(!diags.has_errors(), "unexpected errors");
        let path = m.path.expect("module decl should be present");
        let names: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["app", "auth"]);
    }

    #[test]
    fn parse_module_declaration_missing() {
        // A file without `module` declaration is valid — path is None.
        let (m, diags) = parse("fn foo() {}\n");
        assert!(m.path.is_none());
        assert!(!diags.has_errors());
    }

    #[test]
    fn parse_import_glob() {
        let (m, diags) = parse("use app.services.*\n");
        assert!(!diags.has_errors(), "unexpected errors");
        assert_eq!(m.imports.len(), 1);
        let imp = &m.imports[0];
        let path_segs: Vec<&str> = imp.path.segments.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(path_segs, ["app", "services"]);
        assert_eq!(imp.items, ImportItems::Glob);
    }

    #[test]
    fn parse_import_named_list() {
        let (m, diags) = parse("use core.collections.{List, Map}\n");
        assert!(!diags.has_errors(), "unexpected errors");
        assert_eq!(m.imports.len(), 1);
        let imp = &m.imports[0];
        let path_segs: Vec<&str> = imp.path.segments.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(path_segs, ["core", "collections"]);
        match &imp.items {
            ImportItems::Named(names) => {
                let ns: Vec<&str> = names.iter().map(|n| n.name.name.as_str()).collect();
                assert_eq!(ns, ["List", "Map"]);
            }
            other => panic!("expected Named, got {other:?}"),
        }
    }

    #[test]
    fn parse_import_single_name() {
        let (m, diags) = parse("use app.models.User\n");
        assert!(!diags.has_errors(), "unexpected errors");
        assert_eq!(m.imports.len(), 1);
        let imp = &m.imports[0];
        // With greedy path parsing, `User` is absorbed into the path and items = Module.
        // OR `User` is parsed as a single named import. Both are acceptable; verify consistency.
        // This test documents the actual behaviour.
        match &imp.items {
            ImportItems::Named(names) if names.len() == 1 => {
                assert_eq!(names[0].name.name, "User");
            }
            ImportItems::Module => {
                // Path ends with `User` — also valid per grammar.
                let last = imp.path.segments.last().expect("non-empty path");
                assert_eq!(last.name, "User");
            }
            other => panic!("unexpected import items: {other:?}"),
        }
    }

    #[test]
    fn parse_multi_import_file() {
        let src = "\
module myapp.core\n\
use core.collections.{List, Map}\n\
use app.models.User\n\
use app.services.*\n\
";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "unexpected errors");

        // Module declaration
        let path = m.path.expect("module decl");
        assert_eq!(path.segments[0].name, "myapp");
        assert_eq!(path.segments[1].name, "core");

        // Three imports
        assert_eq!(m.imports.len(), 3);

        // First: named list
        let first = &m.imports[0];
        assert!(matches!(first.items, ImportItems::Named(_)));

        // Last: glob
        let last = &m.imports[2];
        assert_eq!(last.items, ImportItems::Glob);
    }

    #[test]
    fn parser_new_and_diagnostics() {
        let file_id = FileId(0);
        let source = SourceFile::new(file_id, PathBuf::from("x.bock"), String::new());
        let tokens = Lexer::new(&source).tokenize();
        let parser = Parser::new(tokens, &source);
        assert!(!parser.diagnostics().has_errors());
    }

    // ── P2.3: Function declaration tests ─────────────────────────────────────

    #[test]
    fn parse_simple_fn() {
        let (m, diags) = parse("fn greet() {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::Fn(f) = &m.items[0] else {
            panic!("expected Fn")
        };
        assert_eq!(f.name.name, "greet");
        assert!(!f.is_async);
        assert_eq!(f.visibility, Visibility::Private);
        assert!(f.params.is_empty());
        assert!(f.return_type.is_none());
    }

    #[test]
    fn parse_fn_with_params_and_return_type() {
        let (m, diags) = parse("fn add(x: Int, y: Int) -> Int {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.name.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(
            f.params[0].pattern,
            Pattern::Bind {
                id: f.params[0].pattern.node_id(),
                span: f.params[0].pattern.span(),
                name: Ident {
                    name: "x".into(),
                    span: f.params[0].pattern.span()
                },
            }
        );
        assert!(f.return_type.is_some());
        let TypeExpr::Named { path, .. } = f.return_type.as_ref().unwrap() else {
            panic!()
        };
        assert_eq!(path.segments[0].name, "Int");
    }

    #[test]
    fn parse_async_fn() {
        let (m, diags) = parse("async fn fetch() -> String {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert!(f.is_async);
        assert_eq!(f.name.name, "fetch");
    }

    #[test]
    fn parse_fn_with_visibility() {
        let (m, diags) = parse("public fn exposed() {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.visibility, Visibility::Public);
    }

    #[test]
    fn parse_fn_with_generic_params() {
        let (m, diags) = parse("fn identity[T](x: T) -> T {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.generic_params.len(), 1);
        assert_eq!(f.generic_params[0].name.name, "T");
    }

    #[test]
    fn parse_fn_with_generic_bounds() {
        let (m, diags) = parse("fn compare[T: Ord](a: T, b: T) -> Bool {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.generic_params[0].name.name, "T");
        assert_eq!(f.generic_params[0].bounds[0].segments[0].name, "Ord");
    }

    #[test]
    fn parse_fn_with_where_clause() {
        let (m, diags) = parse("fn sorted[T](items: List) -> List where (T: Ord) {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.where_clause.len(), 1);
        assert_eq!(f.where_clause[0].param.name, "T");
        assert_eq!(f.where_clause[0].bounds[0].segments[0].name, "Ord");
    }

    #[test]
    fn parse_fn_with_where_clause_multiple_constraints() {
        let (m, diags) = parse("fn dual[T, U](a: T, b: U) -> Bool where (T: Eq, U: Ord) {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.where_clause.len(), 2);
    }

    #[test]
    fn parse_fn_with_effect_clause() {
        let (m, diags) = parse("fn log_msg(msg: String) with Log {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.effect_clause.len(), 1);
        assert_eq!(f.effect_clause[0].segments[0].name, "Log");
    }

    #[test]
    fn parse_fn_with_multiple_effects() {
        let (m, diags) = parse("fn do_io(path: String) with Io, Log {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.effect_clause.len(), 2);
    }

    #[test]
    fn parse_fn_with_default_param() {
        let (m, diags) = parse("fn greet(name: String, loud: Bool = false) {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert!(f.params[1].default.is_some());
        let Expr::Literal {
            lit: Literal::Bool(false),
            ..
        } = f.params[1].default.as_ref().unwrap()
        else {
            panic!("expected false literal")
        };
    }

    #[test]
    fn parse_fn_with_annotation() {
        let (m, diags) = parse("@test\nfn it_works() {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.annotations.len(), 1);
        assert_eq!(f.annotations[0].name.name, "test");
    }

    // ── P2.3: Record declaration tests ───────────────────────────────────────

    #[test]
    fn parse_simple_record() {
        let (m, diags) = parse("record Point { x: Int, y: Int }\n");
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::Record(r) = &m.items[0] else {
            panic!("expected Record")
        };
        assert_eq!(r.name.name, "Point");
        assert_eq!(r.fields.len(), 2);
        assert_eq!(r.fields[0].name.name, "x");
        assert_eq!(r.fields[1].name.name, "y");
    }

    #[test]
    fn parse_record_with_default_field_values() {
        let (m, diags) = parse("record Config { retries: Int = 3, verbose: Bool = false }\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Record(r) = &m.items[0] else {
            panic!()
        };
        assert_eq!(r.name.name, "Config");
        assert!(r.fields[0].default.is_some());
        let Expr::Literal {
            lit: Literal::Int(n),
            ..
        } = r.fields[0].default.as_ref().unwrap()
        else {
            panic!("expected int literal")
        };
        assert_eq!(n, "3");
        assert!(r.fields[1].default.is_some());
    }

    #[test]
    fn parse_record_with_generic_params() {
        let (m, diags) = parse("record Pair[A, B] { first: A, second: B }\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Record(r) = &m.items[0] else {
            panic!()
        };
        assert_eq!(r.generic_params.len(), 2);
        assert_eq!(r.generic_params[0].name.name, "A");
        assert_eq!(r.generic_params[1].name.name, "B");
    }

    #[test]
    fn parse_record_with_annotation() {
        let (m, diags) = parse("@derive(Equatable)\nrecord User { id: Int, name: String }\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Record(r) = &m.items[0] else {
            panic!()
        };
        assert_eq!(r.annotations.len(), 1);
        assert_eq!(r.annotations[0].name.name, "derive");
    }

    #[test]
    fn parse_record_with_visibility() {
        let (m, diags) = parse("public record Token { kind: Int }\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Record(r) = &m.items[0] else {
            panic!()
        };
        assert_eq!(r.visibility, Visibility::Public);
    }

    #[test]
    fn parse_record_optional_field_type() {
        let (m, diags) = parse("record Profile { name: String, bio: String? }\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Record(r) = &m.items[0] else {
            panic!()
        };
        assert!(matches!(r.fields[1].ty, TypeExpr::Optional { .. }));
    }

    // ── P2.3: Enum declaration tests ─────────────────────────────────────────

    #[test]
    fn parse_enum_unit_variants() {
        let (m, diags) = parse("enum Direction {\n  North\n  South\n  East\n  West\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Enum(e) = &m.items[0] else {
            panic!("expected Enum")
        };
        assert_eq!(e.name.name, "Direction");
        assert_eq!(e.variants.len(), 4);
        for v in &e.variants {
            assert!(matches!(v, EnumVariant::Unit { .. }));
        }
    }

    #[test]
    fn parse_enum_struct_variants() {
        let src = "enum Shape {\n  Circle { radius: Float }\n  Rect { w: Float, h: Float }\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Enum(e) = &m.items[0] else { panic!() };
        assert_eq!(e.variants.len(), 2);
        assert!(matches!(&e.variants[0], EnumVariant::Struct { .. }));
        assert!(matches!(&e.variants[1], EnumVariant::Struct { .. }));
        let EnumVariant::Struct { fields, .. } = &e.variants[0] else {
            unreachable!()
        };
        assert_eq!(fields[0].name.name, "radius");
    }

    #[test]
    fn parse_enum_tuple_variants() {
        let src = "enum Result {\n  Ok(Int)\n  Err(String)\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Enum(e) = &m.items[0] else { panic!() };
        assert_eq!(e.variants.len(), 2);
        assert!(matches!(&e.variants[0], EnumVariant::Tuple { .. }));
        assert!(matches!(&e.variants[1], EnumVariant::Tuple { .. }));
        let EnumVariant::Tuple { tys, .. } = &e.variants[0] else {
            unreachable!()
        };
        assert_eq!(tys.len(), 1);
    }

    #[test]
    fn parse_enum_mixed_variants() {
        let src = "enum Expr {\n  Num(Int)\n  Add { left: Int, right: Int }\n  Unit\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Enum(e) = &m.items[0] else { panic!() };
        assert_eq!(e.variants.len(), 3);
        assert!(matches!(&e.variants[0], EnumVariant::Tuple { .. }));
        assert!(matches!(&e.variants[1], EnumVariant::Struct { .. }));
        assert!(matches!(&e.variants[2], EnumVariant::Unit { .. }));
    }

    #[test]
    fn parse_enum_with_generics() {
        let src = "enum Option[T] {\n  Some(T)\n  None\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Enum(e) = &m.items[0] else { panic!() };
        assert_eq!(e.generic_params.len(), 1);
        assert_eq!(e.generic_params[0].name.name, "T");
        assert_eq!(e.variants.len(), 2);
    }

    #[test]
    fn parse_enum_with_annotation() {
        let src = "@derive(Equatable)\nenum Status {\n  Active\n  Inactive\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Enum(e) = &m.items[0] else { panic!() };
        assert_eq!(e.annotations.len(), 1);
    }

    // ── P2.3: Type expression tests ───────────────────────────────────────────

    #[test]
    fn parse_generic_type_in_field() {
        let (m, diags) = parse("record Container { items: List[Int] }\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Record(r) = &m.items[0] else {
            panic!()
        };
        let TypeExpr::Named { args, .. } = &r.fields[0].ty else {
            panic!()
        };
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn parse_multiple_items_in_file() {
        let src = "fn foo() {}\nrecord Bar { x: Int }\nenum Baz { A\n  B\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(m.items.len(), 3);
        assert!(matches!(m.items[0], Item::Fn(_)));
        assert!(matches!(m.items[1], Item::Record(_)));
        assert!(matches!(m.items[2], Item::Enum(_)));
    }

    // ── P2.4: Class / Trait / Impl tests ─────────────────────────────────────

    #[test]
    fn parse_empty_class() {
        let src = "class Animal {}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Class(c) = &m.items[0] else {
            panic!("expected Class")
        };
        assert_eq!(c.name.name, "Animal");
        assert!(c.base.is_none());
        assert!(c.traits.is_empty());
        assert!(c.fields.is_empty());
        assert!(c.methods.is_empty());
    }

    #[test]
    fn parse_class_with_fields_and_method() {
        let src = "class Point {\n  x: Int\n  y: Int\n  fn distance(self) -> Float { }\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Class(c) = &m.items[0] else {
            panic!()
        };
        assert_eq!(c.name.name, "Point");
        assert_eq!(c.fields.len(), 2);
        assert_eq!(c.fields[0].name.name, "x");
        assert_eq!(c.fields[1].name.name, "y");
        assert_eq!(c.methods.len(), 1);
        assert_eq!(c.methods[0].name.name, "distance");
    }

    #[test]
    fn parse_class_with_inheritance() {
        let src = "class Dog : Animal, Trainable {}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Class(c) = &m.items[0] else {
            panic!()
        };
        assert_eq!(c.name.name, "Dog");
        let base = c.base.as_ref().expect("should have base");
        assert_eq!(base.segments[0].name, "Animal");
        assert_eq!(c.traits.len(), 1);
        assert_eq!(c.traits[0].segments[0].name, "Trainable");
    }

    #[test]
    fn parse_class_with_generics() {
        let src = "class Box[T] {\n  value: T\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Class(c) = &m.items[0] else {
            panic!()
        };
        assert_eq!(c.generic_params.len(), 1);
        assert_eq!(c.generic_params[0].name.name, "T");
        assert_eq!(c.fields.len(), 1);
    }

    #[test]
    fn parse_class_with_annotation() {
        let src = "@derive(Equatable)\nclass User {\n  name: String\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Class(c) = &m.items[0] else {
            panic!()
        };
        assert_eq!(c.annotations.len(), 1);
        assert_eq!(c.annotations[0].name.name, "derive");
    }

    #[test]
    fn parse_public_class() {
        let src = "public class Foo {}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Class(c) = &m.items[0] else {
            panic!()
        };
        assert_eq!(c.visibility, Visibility::Public);
    }

    #[test]
    fn parse_empty_trait() {
        let src = "trait Printable {}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Trait(t) = &m.items[0] else {
            panic!("expected Trait")
        };
        assert_eq!(t.name.name, "Printable");
        assert!(!t.is_platform);
        assert!(t.methods.is_empty());
        assert!(t.associated_types.is_empty());
    }

    #[test]
    fn parse_trait_with_required_and_default_methods() {
        let src = "trait Display {\n  fn show(self) -> String\n  fn debug(self) -> String { }\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Trait(t) = &m.items[0] else {
            panic!()
        };
        assert_eq!(t.methods.len(), 2);
        assert_eq!(t.methods[0].name.name, "show");
        assert_eq!(t.methods[1].name.name, "debug");
    }

    #[test]
    fn parse_trait_with_associated_type() {
        let src = "trait Collection {\n  type Item\n  fn len(self) -> Int\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Trait(t) = &m.items[0] else {
            panic!()
        };
        assert_eq!(t.associated_types.len(), 1);
        assert_eq!(t.associated_types[0].name.name, "Item");
        assert_eq!(t.methods.len(), 1);
    }

    #[test]
    fn parse_trait_associated_type_with_bound() {
        let src = "trait Keyed {\n  type Key: Hashable\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Trait(t) = &m.items[0] else {
            panic!()
        };
        assert_eq!(t.associated_types[0].bounds.len(), 1);
        assert_eq!(t.associated_types[0].bounds[0].segments[0].name, "Hashable");
    }

    #[test]
    fn parse_trait_with_generics() {
        let src = "trait Functor[F] {\n  fn map(self) -> F\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Trait(t) = &m.items[0] else {
            panic!()
        };
        assert_eq!(t.generic_params.len(), 1);
        assert_eq!(t.generic_params[0].name.name, "F");
    }

    #[test]
    fn parse_platform_trait() {
        let src = "platform trait FileSystem {\n  fn read(path: String) -> String\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::PlatformTrait(t) = &m.items[0] else {
            panic!("expected PlatformTrait")
        };
        assert_eq!(t.name.name, "FileSystem");
        assert!(t.is_platform);
        assert_eq!(t.methods.len(), 1);
    }

    #[test]
    fn parse_impl_type() {
        let src = "impl Dog {\n  fn bark(self) -> String { }\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Impl(b) = &m.items[0] else {
            panic!("expected Impl")
        };
        assert!(b.trait_path.is_none());
        assert_eq!(b.methods.len(), 1);
        assert_eq!(b.methods[0].name.name, "bark");
    }

    #[test]
    fn parse_impl_trait_for_type() {
        let src = "impl Printable for Dog {\n  fn show(self) -> String { }\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Impl(b) = &m.items[0] else { panic!() };
        let trait_path = b.trait_path.as_ref().expect("should have trait path");
        assert_eq!(trait_path.segments[0].name, "Printable");
        assert_eq!(b.methods.len(), 1);
    }

    #[test]
    fn parse_impl_with_generics() {
        let src = "impl[T] Display for Box[T] {\n  fn show(self) -> String { }\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Impl(b) = &m.items[0] else { panic!() };
        assert_eq!(b.generic_params.len(), 1);
        assert_eq!(b.generic_params[0].name.name, "T");
        let trait_path = b.trait_path.as_ref().expect("trait path");
        assert_eq!(trait_path.segments[0].name, "Display");
    }

    #[test]
    fn parse_impl_empty_body() {
        let src = "impl Animal {}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Impl(b) = &m.items[0] else { panic!() };
        assert!(b.methods.is_empty());
    }

    #[test]
    fn parse_mixed_items_with_class_trait_impl() {
        let src = concat!(
            "trait Greet {\n  fn hello(self) -> String\n}\n",
            "class Cat {}\n",
            "impl Greet for Cat {\n  fn hello(self) -> String { }\n}\n",
        );
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(m.items.len(), 3);
        assert!(matches!(m.items[0], Item::Trait(_)));
        assert!(matches!(m.items[1], Item::Class(_)));
        assert!(matches!(m.items[2], Item::Impl(_)));
    }

    // ─── Expression parsing tests (P2.5) ─────────────────────────────────────

    /// Helper to parse a function body and extract the tail expression.
    fn parse_fn_body(src: &str) -> (Expr, DiagnosticBag) {
        let src = format!("fn test() {{\n{src}\n}}");
        let (m, diags) = parse(&src);
        let fn_decl = match m.items.first().expect("expected fn") {
            Item::Fn(f) => f.clone(),
            _ => panic!("expected fn item"),
        };
        let body = fn_decl.body.expect("expected function body");
        let tail = body.tail.expect("expected tail expression");
        (*tail, diags)
    }

    /// Helper to parse an expression directly (no fn wrapper).
    fn parse_expr_str(src: &str) -> (Expr, DiagnosticBag) {
        parse_fn_body(src)
    }

    #[test]
    fn expr_integer_literal() {
        let (e, diags) = parse_expr_str("42");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Literal { lit: Literal::Int(ref s), .. } if s == "42"));
    }

    #[test]
    fn expr_float_literal() {
        let (e, diags) = parse_expr_str("3.14");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            e,
            Expr::Literal {
                lit: Literal::Float(_),
                ..
            }
        ));
    }

    #[test]
    fn expr_bool_literal() {
        let (e, diags) = parse_expr_str("true");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            e,
            Expr::Literal {
                lit: Literal::Bool(true),
                ..
            }
        ));

        let (e2, _) = parse_expr_str("false");
        assert!(matches!(
            e2,
            Expr::Literal {
                lit: Literal::Bool(false),
                ..
            }
        ));
    }

    #[test]
    fn expr_string_literal() {
        let (e, diags) = parse_expr_str(r#""hello""#);
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            e,
            Expr::Literal {
                lit: Literal::String(_),
                ..
            }
        ));
    }

    #[test]
    fn expr_identifier() {
        let (e, diags) = parse_expr_str("foo");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Identifier { ref name, .. } if name.name == "foo"));
    }

    #[test]
    fn expr_binary_add() {
        let (e, diags) = parse_expr_str("1 + 2");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Binary { op: BinOp::Add, .. }));
    }

    #[test]
    fn expr_binary_precedence_mul_over_add() {
        // `1 + 2 * 3` should parse as `1 + (2 * 3)`
        let (e, diags) = parse_expr_str("1 + 2 * 3");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Binary {
                op: BinOp::Add,
                right,
                ..
            } => {
                assert!(matches!(*right, Expr::Binary { op: BinOp::Mul, .. }));
            }
            _ => panic!("expected Add binary expr, got {e:?}"),
        }
    }

    #[test]
    fn expr_binary_left_associative() {
        // `1 - 2 - 3` should parse as `(1 - 2) - 3`
        let (e, diags) = parse_expr_str("1 - 2 - 3");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Binary {
                op: BinOp::Sub,
                left,
                right,
                ..
            } => {
                assert!(matches!(*left, Expr::Binary { op: BinOp::Sub, .. }));
                assert!(matches!(*right, Expr::Literal { .. }));
            }
            _ => panic!("expected Sub binary expr"),
        }
    }

    #[test]
    fn expr_power_right_associative() {
        // `2 ** 3 ** 4` should parse as `2 ** (3 ** 4)`
        let (e, diags) = parse_expr_str("2 ** 3 ** 4");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Binary {
                op: BinOp::Pow,
                left,
                right,
                ..
            } => {
                assert!(matches!(*left, Expr::Literal { .. }));
                assert!(matches!(*right, Expr::Binary { op: BinOp::Pow, .. }));
            }
            _ => panic!("expected Pow binary expr"),
        }
    }

    #[test]
    fn expr_comparison_operators() {
        for (src, expected_op) in [
            ("a == b", BinOp::Eq),
            ("a != b", BinOp::Ne),
            ("a < b", BinOp::Lt),
            ("a > b", BinOp::Gt),
            ("a <= b", BinOp::Le),
            ("a >= b", BinOp::Ge),
        ] {
            let (e, diags) = parse_expr_str(src);
            assert!(!diags.has_errors(), "errors for {src}: {diags:?}");
            assert!(
                matches!(&e, Expr::Binary { op, .. } if *op == expected_op),
                "{src} expected {expected_op:?}"
            );
        }
    }

    #[test]
    fn expr_logical_and_or() {
        let (e, diags) = parse_expr_str("a && b");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Binary { op: BinOp::And, .. }));

        let (e2, _) = parse_expr_str("a || b");
        assert!(matches!(e2, Expr::Binary { op: BinOp::Or, .. }));
    }

    #[test]
    fn expr_and_binds_tighter_than_or() {
        // `a || b && c` should parse as `a || (b && c)`
        let (e, diags) = parse_expr_str("a || b && c");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Binary {
                op: BinOp::Or,
                right,
                ..
            } => {
                assert!(matches!(*right, Expr::Binary { op: BinOp::And, .. }));
            }
            _ => panic!("expected Or expr"),
        }
    }

    #[test]
    fn expr_assignment() {
        let (e, diags) = parse_expr_str("x = 5");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            e,
            Expr::Assign {
                op: AssignOp::Assign,
                ..
            }
        ));
    }

    #[test]
    fn expr_compound_assignment() {
        for (src, expected_op) in [
            ("x += 1", AssignOp::AddAssign),
            ("x -= 1", AssignOp::SubAssign),
            ("x *= 2", AssignOp::MulAssign),
            ("x /= 2", AssignOp::DivAssign),
            ("x %= 3", AssignOp::RemAssign),
        ] {
            let (e, diags) = parse_expr_str(src);
            assert!(!diags.has_errors(), "errors for {src}: {diags:?}");
            assert!(
                matches!(&e, Expr::Assign { op, .. } if *op == expected_op),
                "expected {expected_op:?} for {src}"
            );
        }
    }

    #[test]
    fn expr_unary_neg() {
        let (e, diags) = parse_expr_str("-x");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            e,
            Expr::Unary {
                op: UnaryOp::Neg,
                ..
            }
        ));
    }

    #[test]
    fn expr_unary_not() {
        let (e, diags) = parse_expr_str("!flag");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            e,
            Expr::Unary {
                op: UnaryOp::Not,
                ..
            }
        ));
    }

    #[test]
    fn expr_unary_bitnot() {
        let (e, diags) = parse_expr_str("~x");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            e,
            Expr::Unary {
                op: UnaryOp::BitNot,
                ..
            }
        ));
    }

    #[test]
    fn expr_try_operator() {
        let (e, diags) = parse_expr_str("result?");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Try { .. }));
    }

    #[test]
    fn expr_field_access() {
        let (e, diags) = parse_expr_str("obj.field");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::FieldAccess { ref field, .. } if field.name == "field"));
    }

    #[test]
    fn expr_method_call() {
        let (e, diags) = parse_expr_str("obj.method(1, 2)");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::MethodCall { ref method, .. } if method.name == "method"));
    }

    #[test]
    fn expr_function_call() {
        let (e, diags) = parse_expr_str("foo(1, 2, 3)");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Call { args, .. } => assert_eq!(args.len(), 3),
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn expr_labeled_arg() {
        let (e, diags) = parse_expr_str("foo(x: 1, y: 2)");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Call { args, .. } => {
                assert_eq!(args.len(), 2);
                assert_eq!(args[0].label.as_ref().map(|i| i.name.as_str()), Some("x"));
                assert_eq!(args[1].label.as_ref().map(|i| i.name.as_str()), Some("y"));
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn expr_index_access() {
        let (e, diags) = parse_expr_str("arr[0]");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Index { .. }));
    }

    #[test]
    fn expr_postfix_chain() {
        // `obj.method(arg).field[0]?`
        let (e, diags) = parse_expr_str("obj.method(arg).field");
        assert!(!diags.has_errors(), "{diags:?}");
        // outer: FieldAccess
        match e {
            Expr::FieldAccess {
                ref field,
                ref object,
                ..
            } => {
                assert_eq!(field.name, "field");
                assert!(matches!(**object, Expr::MethodCall { .. }));
            }
            _ => panic!("expected FieldAccess, got {e:?}"),
        }
    }

    #[test]
    fn expr_deep_postfix_chain() {
        let (e, diags) = parse_expr_str("a.b(c).d[0]?");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Try { .. }));
    }

    #[test]
    fn expr_pipe_operator() {
        let (e, diags) = parse_expr_str("data |> parse");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Pipe { .. }));
    }

    #[test]
    fn expr_pipe_chain() {
        // `a |> b |> c` should be left-assoc: `(a |> b) |> c`
        let (e, diags) = parse_expr_str("a |> b |> c");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Pipe { left, .. } => {
                assert!(matches!(*left, Expr::Pipe { .. }));
            }
            _ => panic!("expected Pipe"),
        }
    }

    #[test]
    fn expr_compose_operator() {
        let (e, diags) = parse_expr_str("parse >> validate");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Compose { .. }));
    }

    #[test]
    fn expr_range_exclusive() {
        let (e, diags) = parse_expr_str("1..10");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            e,
            Expr::Range {
                inclusive: false,
                ..
            }
        ));
    }

    #[test]
    fn expr_range_inclusive() {
        let (e, diags) = parse_expr_str("1..=10");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            e,
            Expr::Range {
                inclusive: true,
                ..
            }
        ));
    }

    #[test]
    fn expr_bitwise_operators() {
        let cases = [
            ("a & b", BinOp::BitAnd),
            ("a | b", BinOp::BitOr),
            ("a ^ b", BinOp::BitXor),
        ];
        for (src, op) in cases {
            let (e, diags) = parse_expr_str(src);
            assert!(!diags.has_errors(), "errors for {src}: {diags:?}");
            assert!(matches!(&e, Expr::Binary { op: actual, .. } if *actual == op));
        }
    }

    #[test]
    fn shl_is_parse_error() {
        // `<<` is not a binary operator; it should not parse as infix.
        // The parser will parse `a` then stop at `<<`, leaving it unconsumed.
        // We verify it did NOT produce a Binary node with shift.
        let (e, _) = parse_expr_str("a << 2");
        assert!(
            !matches!(&e, Expr::Binary { .. }),
            "`<<` must not be parsed as infix binary operator"
        );
    }

    #[test]
    fn compose_still_works() {
        // `>>` remains function composition
        let (e, diags) = parse_expr_str("f >> g");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Compose { .. }));
    }

    #[test]
    fn precedence_add_mul_after_renumber() {
        // a + b * c should parse as a + (b * c) — mul binds tighter than add
        let (e, diags) = parse_expr_str("a + b * c");
        assert!(!diags.has_errors(), "{diags:?}");
        // Top-level should be Add
        match &e {
            Expr::Binary { op, right, .. } => {
                assert_eq!(*op, BinOp::Add);
                assert!(
                    matches!(right.as_ref(), Expr::Binary { op: inner_op, .. } if *inner_op == BinOp::Mul),
                    "right side of Add should be Mul"
                );
            }
            _ => panic!("expected Binary(Add) at top level"),
        }
    }

    #[test]
    fn expr_placeholder() {
        let (e, diags) = parse_expr_str("_");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Placeholder { .. }));
    }

    #[test]
    fn expr_list_literal() {
        let (e, diags) = parse_expr_str("[1, 2, 3]");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::ListLiteral { elems, .. } => assert_eq!(elems.len(), 3),
            _ => panic!("expected ListLiteral"),
        }
    }

    #[test]
    fn expr_empty_list() {
        let (e, diags) = parse_expr_str("[]");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::ListLiteral { ref elems, .. } if elems.is_empty()));
    }

    #[test]
    fn expr_tuple_literal() {
        let (e, diags) = parse_expr_str("(1, 2, 3)");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::TupleLiteral { elems, .. } => assert_eq!(elems.len(), 3),
            _ => panic!("expected TupleLiteral, got {e:?}"),
        }
    }

    #[test]
    fn expr_unit_literal() {
        let (e, diags) = parse_expr_str("()");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            e,
            Expr::Literal {
                lit: Literal::Unit,
                ..
            }
        ));
    }

    #[test]
    fn expr_set_literal() {
        let (e, diags) = parse_expr_str(r#"#{"a", "b"}"#);
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::SetLiteral { elems, .. } => assert_eq!(elems.len(), 2),
            _ => panic!("expected SetLiteral"),
        }
    }

    #[test]
    fn expr_map_literal() {
        let (e, diags) = parse_expr_str(r#"{"key": "value"}"#);
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::MapLiteral { entries, .. } => assert_eq!(entries.len(), 1),
            _ => panic!("expected MapLiteral, got {e:?}"),
        }
    }

    #[test]
    fn empty_map_literal_with_type_annotation() {
        // FC-15: `let m: Map[String, Int] = {}` should parse as an empty MapLiteral
        let src = "fn test() {\nlet m: Map[String, Int] = {}\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let f = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            other => panic!("expected Fn, got {other:?}"),
        };
        let body = f.body.as_ref().expect("expected body");
        let stmt = body.stmts.first().expect("expected a statement");
        match stmt {
            Stmt::Let(let_stmt) => match &let_stmt.value {
                Expr::MapLiteral { entries, .. } => assert!(entries.is_empty()),
                other => panic!("expected MapLiteral, got {other:?}"),
            },
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn empty_braces_without_map_annotation_is_block() {
        // `let x = {}` without Map annotation should still be a block
        let src = "fn test() {\nlet x = {}\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let f = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            other => panic!("expected Fn, got {other:?}"),
        };
        let body = f.body.as_ref().expect("expected body");
        let stmt = body.stmts.first().expect("expected a statement");
        match stmt {
            Stmt::Let(let_stmt) => assert!(
                matches!(&let_stmt.value, Expr::Block { .. }),
                "expected Block, got {:?}",
                let_stmt.value
            ),
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn if_empty_block_still_works() {
        // `if (cond) {}` should still parse as an if with an empty block
        let (e, diags) = parse_expr_str("if (true) {}");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::If { .. }));
    }

    #[test]
    fn expr_block_is_expression() {
        let src = "fn test() {\n{ let x = 1\nx }\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        // The body should have a tail that is a Block expression
        let body = fn_decl.body.as_ref().expect("expected function body");
        let tail = body.tail.as_ref().expect("expected tail");
        assert!(matches!(**tail, Expr::Block { .. }));
    }

    #[test]
    fn expr_if_expression() {
        let (e, diags) = parse_expr_str("if (cond) { a } else { b }");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::If {
                let_pattern,
                else_block,
                ..
            } => {
                assert!(let_pattern.is_none());
                assert!(else_block.is_some());
            }
            _ => panic!("expected If expr, got {e:?}"),
        }
    }

    #[test]
    fn expr_if_no_else() {
        let (e, diags) = parse_expr_str("if (x > 0) { foo() }");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            e,
            Expr::If {
                else_block: None,
                ..
            }
        ));
    }

    #[test]
    fn expr_if_let() {
        let (e, diags) = parse_expr_str("if (let Some(v) = opt) { v }");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::If {
                let_pattern: Some(p),
                ..
            } => {
                assert!(matches!(p, Pattern::Constructor { .. }));
            }
            _ => panic!("expected if-let, got {e:?}"),
        }
    }

    #[test]
    fn expr_if_else_if_chain() {
        let (e, diags) = parse_expr_str("if (a) { 1 } else if (b) { 2 } else { 3 }");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::If {
                else_block: Some(else_e),
                ..
            } => {
                assert!(matches!(*else_e, Expr::If { .. }));
            }
            _ => panic!("expected if-else-if chain"),
        }
    }

    #[test]
    fn expr_match() {
        let src = "match val {\n  0 => zero\n  n => other\n}";
        let (e, diags) = parse_expr_str(src);
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Match { arms, .. } => assert_eq!(arms.len(), 2),
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn expr_match_with_guard() {
        let src = "match x {\n  n if (n > 0) => pos\n  _ => other\n}";
        let (e, diags) = parse_expr_str(src);
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Match { arms, .. } => {
                assert!(arms[0].guard.is_some());
                assert!(arms[1].guard.is_none());
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn expr_lambda_single_param() {
        let (e, diags) = parse_expr_str("(x) => x * 2");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Lambda { params, body, .. } => {
                assert_eq!(params.len(), 1);
                assert!(matches!(*body, Expr::Binary { op: BinOp::Mul, .. }));
            }
            _ => panic!("expected Lambda, got {e:?}"),
        }
    }

    #[test]
    fn expr_lambda_no_params() {
        let (e, diags) = parse_expr_str("() => 42");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Lambda { params, .. } => assert_eq!(params.len(), 0),
            _ => panic!("expected Lambda"),
        }
    }

    #[test]
    fn expr_lambda_block_body() {
        let (e, diags) = parse_expr_str("(a, b) => { a + b }");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Lambda { params, body, .. } => {
                assert_eq!(params.len(), 2);
                assert!(matches!(*body, Expr::Block { .. }));
            }
            _ => panic!("expected Lambda"),
        }
    }

    #[test]
    fn expr_lambda_typed_params() {
        let (e, diags) = parse_expr_str("(x: Int) => x");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Lambda { params, .. } => {
                assert_eq!(params.len(), 1);
                assert!(params[0].ty.is_some());
            }
            _ => panic!("expected Lambda"),
        }
    }

    #[test]
    fn expr_return() {
        let (e, diags) = parse_expr_str("return 42");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Return { value: Some(_), .. }));
    }

    #[test]
    fn expr_return_void() {
        // `return` inside a block followed by `}`
        let src = "fn test() {\nreturn\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        // `return` is parsed as a tail or statement
        let body = fn_decl.body.as_ref().expect("expected function body");
        let has_return = body
            .tail
            .as_ref()
            .map(|t| matches!(**t, Expr::Return { .. }))
            .or_else(|| {
                body.stmts
                    .last()
                    .map(|s| matches!(s, Stmt::Expr(Expr::Return { .. })))
            })
            .unwrap_or(false);
        assert!(has_return, "expected return expr in body");
    }

    #[test]
    fn expr_await() {
        let (e, diags) = parse_expr_str("await foo()");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Await { .. }));
    }

    #[test]
    fn expr_await_postfix() {
        let (e, diags) = parse_expr_str("foo().await");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Await { .. }));
    }

    #[test]
    fn expr_unreachable() {
        let (e, diags) = parse_expr_str("unreachable");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Unreachable { .. }));
    }

    #[test]
    fn expr_is_simple_type() {
        let (e, diags) = parse_expr_str("value is Int");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Is { type_expr, .. } => {
                if let TypeExpr::Named { path, args, .. } = type_expr {
                    assert_eq!(path.segments[0].name, "Int");
                    assert!(args.is_empty());
                } else {
                    panic!("expected Named type_expr, got {type_expr:?}");
                }
            }
            _ => panic!("expected Expr::Is, got {e:?}"),
        }
    }

    #[test]
    fn expr_is_generic_args_preserved() {
        let (e, diags) = parse_expr_str("x is Option[Int]");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Is { type_expr, .. } => {
                if let TypeExpr::Named { path, args, .. } = type_expr {
                    assert_eq!(path.segments[0].name, "Option");
                    assert_eq!(args.len(), 1);
                } else {
                    panic!("expected Named type_expr, got {type_expr:?}");
                }
            }
            _ => panic!("expected Expr::Is, got {e:?}"),
        }
    }

    #[test]
    fn expr_is_multi_arg_generic() {
        let (e, diags) = parse_expr_str("x is Result[String, Error]");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Is { type_expr, .. } => {
                if let TypeExpr::Named { path, args, .. } = type_expr {
                    assert_eq!(path.segments[0].name, "Result");
                    assert_eq!(args.len(), 2);
                } else {
                    panic!("expected Named type_expr, got {type_expr:?}");
                }
            }
            _ => panic!("expected Expr::Is, got {e:?}"),
        }
    }

    #[test]
    fn expr_is_nested_generic() {
        let (e, diags) = parse_expr_str("x is List[List[Int]]");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Is { type_expr, .. } => {
                if let TypeExpr::Named { path, args, .. } = type_expr {
                    assert_eq!(path.segments[0].name, "List");
                    assert_eq!(args.len(), 1);
                    // Check nested generic
                    if let TypeExpr::Named {
                        path: inner,
                        args: inner_args,
                        ..
                    } = &args[0]
                    {
                        assert_eq!(inner.segments[0].name, "List");
                        assert_eq!(inner_args.len(), 1);
                    } else {
                        panic!("expected nested Named type_expr");
                    }
                } else {
                    panic!("expected Named type_expr, got {type_expr:?}");
                }
            }
            _ => panic!("expected Expr::Is, got {e:?}"),
        }
    }

    #[test]
    fn expr_record_construct() {
        let (e, diags) = parse_expr_str("User { id: 1, name }");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::RecordConstruct {
                path,
                fields,
                spread,
                ..
            } => {
                assert_eq!(path.segments[0].name, "User");
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].name.name, "id");
                assert!(fields[0].value.is_some()); // `id: 1`
                assert_eq!(fields[1].name.name, "name");
                assert!(fields[1].value.is_none()); // shorthand
                assert!(spread.is_none());
            }
            _ => panic!("expected RecordConstruct, got {e:?}"),
        }
    }

    #[test]
    fn expr_record_construct_with_spread() {
        let (e, diags) = parse_expr_str("User { name, ..defaults }");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::RecordConstruct { spread, .. } => {
                assert!(spread.is_some());
            }
            _ => panic!("expected RecordConstruct"),
        }
    }

    #[test]
    fn expr_let_statement_in_block() {
        let src = "fn test() {\nlet x = 42\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        let body = fn_decl.body.as_ref().expect("expected function body");
        assert_eq!(body.stmts.len(), 1);
        assert!(matches!(body.stmts[0], Stmt::Let(_)));
        assert!(body.tail.is_some());
    }

    #[test]
    fn expr_let_mut_in_block() {
        let src = "fn test() {\nlet mut x = 0\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        let body = fn_decl.body.as_ref().expect("expected function body");
        assert!(
            matches!(&body.stmts[0], Stmt::Let(l) if matches!(l.pattern, Pattern::MutBind { .. }))
        );
    }

    #[test]
    fn expr_for_loop_in_block() {
        let src = "fn test() {\nfor x in items { foo(x) }\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        assert!(matches!(
            &fn_decl.body.as_ref().unwrap().stmts[0],
            Stmt::For(_)
        ));
    }

    #[test]
    fn expr_while_loop_in_block() {
        let src = "fn test() {\nwhile (x > 0) { x -= 1 }\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        assert!(matches!(
            &fn_decl.body.as_ref().unwrap().stmts[0],
            Stmt::While(_)
        ));
    }

    #[test]
    fn expr_loop_in_block() {
        let src = "fn test() {\nloop { break }\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        // Loop as last item in block becomes the tail expression
        assert!(matches!(
            fn_decl.body.as_ref().unwrap().tail.as_deref(),
            Some(Expr::Loop { .. })
        ));
    }

    #[test]
    fn expr_complex_expression() {
        // `data |> parse |> validate`
        let (e, diags) = parse_expr_str("data |> parse |> validate");
        assert!(!diags.has_errors(), "{diags:?}");
        // Left-assoc: (data |> parse) |> validate
        match e {
            Expr::Pipe { left, .. } => {
                assert!(matches!(*left, Expr::Pipe { .. }));
            }
            _ => panic!("expected Pipe chain"),
        }
    }

    #[test]
    fn expr_nested_binary() {
        // `(a + b) * c`
        let (e, diags) = parse_expr_str("(a + b) * c");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Binary {
                op: BinOp::Mul,
                left,
                ..
            } => {
                assert!(matches!(*left, Expr::Binary { op: BinOp::Add, .. }));
            }
            _ => panic!("expected Mul expr"),
        }
    }

    #[test]
    fn expr_pattern_constructor_in_match() {
        let src = "match opt {\n  Some(x) => x\n  None => 0\n}";
        let (e, diags) = parse_expr_str(src);
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Match { arms, .. } => {
                assert!(matches!(&arms[0].pattern, Pattern::Constructor { .. }));
                assert!(matches!(&arms[1].pattern, Pattern::Constructor { .. }));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn expr_pattern_wildcard_in_match() {
        let src = "match x {\n  _ => 0\n}";
        let (e, diags) = parse_expr_str(src);
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Match { arms, .. } => {
                assert!(matches!(&arms[0].pattern, Pattern::Wildcard { .. }));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn expr_pattern_or() {
        let src = "match x {\n  1 | 2 => few\n  _ => many\n}";
        let (e, diags) = parse_expr_str(src);
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::Match { arms, .. } => {
                assert!(matches!(&arms[0].pattern, Pattern::Or { .. }));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn expr_if_as_primary_expression() {
        // `let x = if (cond) { a } else { b }`
        let src = "fn test() {\nlet x = if (cond) { a } else { b }\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        // First stmt should be let with if-expr as value
        match &fn_decl.body.as_ref().unwrap().stmts[0] {
            Stmt::Let(l) => {
                assert!(
                    matches!(l.value, Expr::If { .. }),
                    "expected If as let value"
                );
            }
            _ => panic!("expected Let stmt"),
        }
    }

    #[test]
    fn expr_match_as_primary_expression() {
        // `let x = match val { 1 => a  _ => b }` (in fn body)
        let src = "fn test() {\nlet x = match val {\n1 => a\n_ => b\n}\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        match &fn_decl.body.as_ref().unwrap().stmts[0] {
            Stmt::Let(l) => {
                assert!(
                    matches!(l.value, Expr::Match { .. }),
                    "expected Match as let value"
                );
            }
            _ => panic!("expected Let stmt"),
        }
    }

    #[test]
    fn expr_loop_as_primary_expression() {
        // `let x = loop { break 42 }`
        let src = "fn test() {\nlet x = loop {\nbreak 42\n}\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        match &fn_decl.body.as_ref().unwrap().stmts[0] {
            Stmt::Let(l) => {
                assert!(
                    matches!(l.value, Expr::Loop { .. }),
                    "expected Loop as let value"
                );
            }
            _ => panic!("expected Let stmt"),
        }
    }

    #[test]
    fn expr_loop_as_function_arg() {
        let src = "fn test() {\nfoo(loop { break 1 })\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        // The loop-as-arg should be the tail expression (a Call whose arg is Loop)
        let tail = fn_decl
            .body
            .as_ref()
            .unwrap()
            .tail
            .as_ref()
            .expect("expected tail");
        match tail.as_ref() {
            Expr::Call { args, .. } => {
                assert!(
                    matches!(args[0].value, Expr::Loop { .. }),
                    "expected Loop as arg"
                );
            }
            _ => panic!("expected Call expr"),
        }
    }

    #[test]
    fn stmt_let_with_type_annotation() {
        let src = "fn test() {\nlet x: Int = 42\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        match &fn_decl.body.as_ref().unwrap().stmts[0] {
            Stmt::Let(l) => {
                assert!(l.ty.is_some(), "expected type annotation on let");
            }
            _ => panic!("expected Let stmt"),
        }
    }

    #[test]
    fn stmt_let_with_tuple_destructuring() {
        let src = "fn test() {\nlet (x, y) = get_point()\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        match &fn_decl.body.as_ref().unwrap().stmts[0] {
            Stmt::Let(l) => {
                assert!(
                    matches!(l.pattern, Pattern::Tuple { .. }),
                    "expected tuple pattern, got {:?}",
                    l.pattern
                );
            }
            _ => panic!("expected Let stmt"),
        }
    }

    #[test]
    fn stmt_guard_in_block() {
        let src = "fn test() {\nguard (x > 0) else { return }\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        match &fn_decl.body.as_ref().unwrap().stmts[0] {
            Stmt::Guard(g) => {
                assert!(
                    !g.else_block.stmts.is_empty()
                        || g.else_block.tail.is_some()
                        || !g.else_block.stmts.is_empty(),
                    "expected non-empty else block"
                );
            }
            _ => panic!(
                "expected Guard stmt, got {:?}",
                fn_decl.body.as_ref().unwrap().stmts[0]
            ),
        }
    }

    #[test]
    fn stmt_handling_block_with_multiple_bindings() {
        let src = "fn test() {\nhandling (Log with logger, Clock with mock) {\ndo_work()\n}\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        match &fn_decl.body.as_ref().unwrap().stmts[0] {
            Stmt::Handling(h) => {
                assert_eq!(h.handlers.len(), 2, "expected 2 handler bindings");
            }
            _ => panic!(
                "expected Handling stmt, got {:?}",
                fn_decl.body.as_ref().unwrap().stmts[0]
            ),
        }
    }

    #[test]
    fn stmt_handling_block_single_binding() {
        let src = "fn test() {\nhandling (Log with handler) {\ndo_work()\n}\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        match &fn_decl.body.as_ref().unwrap().stmts[0] {
            Stmt::Handling(h) => {
                assert_eq!(h.handlers.len(), 1);
            }
            _ => panic!("expected Handling stmt"),
        }
    }

    #[test]
    fn stmt_continuation_operator_at_end_of_line() {
        // Operator at end of line: `a +\n  b` should parse as `a + b`
        let src = "fn test() {\nlet x = a +\n  b\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        match &fn_decl.body.as_ref().unwrap().stmts[0] {
            Stmt::Let(l) => {
                assert!(
                    matches!(l.value, Expr::Binary { op: BinOp::Add, .. }),
                    "expected binary Add across lines"
                );
            }
            _ => panic!("expected Let stmt"),
        }
    }

    #[test]
    fn stmt_continuation_pipe_at_start_of_next_line() {
        // `|>` at start of next line continues the expression
        let src = "fn test() {\nlet x = data\n  |> transform\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        match &fn_decl.body.as_ref().unwrap().stmts[0] {
            Stmt::Let(l) => {
                assert!(
                    matches!(l.value, Expr::Pipe { .. }),
                    "expected Pipe across lines"
                );
            }
            _ => panic!("expected Let stmt"),
        }
    }

    #[test]
    fn stmt_continuation_dot_at_start_of_next_line() {
        // `.method()` at start of next line continues the expression
        let src = "fn test() {\nlet x = obj\n  .field\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        match &fn_decl.body.as_ref().unwrap().stmts[0] {
            Stmt::Let(l) => {
                assert!(
                    matches!(l.value, Expr::FieldAccess { .. }),
                    "expected FieldAccess across lines"
                );
            }
            _ => panic!("expected Let stmt"),
        }
    }

    #[test]
    fn stmt_continuation_else_on_next_line() {
        // `else` on a new line after `}` continues the if-expression (spec §3.2 rule 8)
        let src = "fn test() -> Int {\nif (true) { 1 }\nelse { 2 }\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        // The if-else is the block's tail expression
        let tail = fn_decl
            .body
            .as_ref()
            .unwrap()
            .tail
            .as_ref()
            .expect("expected tail expr");
        match tail.as_ref() {
            Expr::If { else_block, .. } => {
                assert!(else_block.is_some(), "expected else block across lines");
            }
            other => panic!("expected If expr, got {other:?}"),
        }
    }

    #[test]
    fn stmt_continuation_else_if_on_next_line() {
        // `else if` chain across lines
        let src = "fn test() -> Int {\nif (true) { 1 }\nelse if (false) { 2 }\nelse { 3 }\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        let tail = fn_decl
            .body
            .as_ref()
            .unwrap()
            .tail
            .as_ref()
            .expect("expected tail expr");
        match tail.as_ref() {
            Expr::If { else_block, .. } => {
                let inner = else_block.as_ref().expect("expected else-if chain");
                assert!(
                    matches!(inner.as_ref(), Expr::If { .. }),
                    "expected nested If in else-if chain"
                );
            }
            other => panic!("expected If expr, got {other:?}"),
        }
    }

    #[test]
    fn stmt_semicolon_separates_statements() {
        // Semicolons always terminate, even on same line
        let src = "fn test() {\nlet x = 1; let y = 2\nx\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        assert_eq!(
            fn_decl.body.as_ref().unwrap().stmts.len(),
            2,
            "expected 2 stmts from semicolon-separated line"
        );
    }

    #[test]
    fn stmt_loop_break_with_value() {
        // `loop { break value }` — break can carry a value
        let src = "fn test() {\nloop { break 42 }\n}";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        // Loop as last item becomes the tail expression
        match fn_decl.body.as_ref().unwrap().tail.as_deref() {
            Some(Expr::Loop { body, .. }) => {
                // The loop body should contain a Break with a value
                let has_break = body
                    .stmts
                    .iter()
                    .any(|s| matches!(s, Stmt::Expr(Expr::Break { value: Some(_), .. })))
                    || body
                        .tail
                        .as_ref()
                        .is_some_and(|t| matches!(**t, Expr::Break { value: Some(_), .. }));
                assert!(has_break, "expected break with value in loop body");
            }
            _ => panic!("expected Loop tail expression"),
        }
    }

    // ── P2.7: Pattern parsing tests ──────────────────────────────────────────

    fn parse_pat(src: &str) -> (Pattern, DiagnosticBag) {
        // Wrap in a match expression to trigger pattern parsing.
        let wrapped = format!("fn f() {{ match x {{\n{src}\n}} }}");
        let (m, diags) = parse(&wrapped);
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!("expected fn"),
        };
        let mat = match fn_decl.body.as_ref().unwrap().tail.as_deref() {
            Some(Expr::Match { arms, .. }) => arms.clone(),
            _ => match fn_decl.body.as_ref().unwrap().stmts.first() {
                Some(Stmt::Expr(Expr::Match { arms, .. })) => arms.clone(),
                _ => panic!("expected match expression"),
            },
        };
        let pat = mat
            .into_iter()
            .next()
            .expect("expected at least one arm")
            .pattern;
        (pat, diags)
    }

    #[test]
    fn pattern_wildcard() {
        let (pat, diags) = parse_pat("_ => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(pat, Pattern::Wildcard { .. }));
    }

    #[test]
    fn pattern_bind() {
        let (pat, diags) = parse_pat("name => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(pat, Pattern::Bind { .. }));
        if let Pattern::Bind { name, .. } = pat {
            assert_eq!(name.name, "name");
        }
    }

    #[test]
    fn pattern_mut_bind() {
        let (pat, diags) = parse_pat("mut x => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(pat, Pattern::MutBind { .. }));
        if let Pattern::MutBind { name, .. } = pat {
            assert_eq!(name.name, "x");
        }
    }

    #[test]
    fn pattern_literal_int() {
        let (pat, diags) = parse_pat("42 => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            pat,
            Pattern::Literal {
                lit: Literal::Int(_),
                ..
            }
        ));
    }

    #[test]
    fn pattern_literal_string() {
        let (pat, diags) = parse_pat(r#""hello" => 1"#);
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            pat,
            Pattern::Literal {
                lit: Literal::String(_),
                ..
            }
        ));
    }

    #[test]
    fn pattern_literal_bool_true() {
        let (pat, diags) = parse_pat("true => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            pat,
            Pattern::Literal {
                lit: Literal::Bool(true),
                ..
            }
        ));
    }

    #[test]
    fn pattern_literal_bool_false() {
        let (pat, diags) = parse_pat("false => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            pat,
            Pattern::Literal {
                lit: Literal::Bool(false),
                ..
            }
        ));
    }

    #[test]
    fn pattern_constructor_some() {
        let (pat, diags) = parse_pat("Some(x) => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        if let Pattern::Constructor { fields, .. } = pat {
            assert_eq!(fields.len(), 1);
            assert!(matches!(fields[0], Pattern::Bind { .. }));
        } else {
            panic!("expected Constructor pattern");
        }
    }

    #[test]
    fn pattern_constructor_err() {
        let (pat, diags) = parse_pat("Err(e) => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(pat, Pattern::Constructor { .. }));
    }

    #[test]
    fn pattern_record_shorthand() {
        let (pat, diags) = parse_pat("Point { x, y } => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        if let Pattern::Record { fields, rest, .. } = pat {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].name.name, "x");
            assert!(
                fields[0].pattern.is_none(),
                "shorthand should have no sub-pattern"
            );
            assert_eq!(fields[1].name.name, "y");
            assert!(!rest, "no rest expected");
        } else {
            panic!("expected Record pattern");
        }
    }

    #[test]
    fn pattern_record_with_rename() {
        let (pat, diags) = parse_pat("User { name: n, age: a } => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        if let Pattern::Record { fields, rest, .. } = pat {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].name.name, "name");
            assert!(fields[0].pattern.is_some());
            assert!(!rest);
        } else {
            panic!("expected Record pattern");
        }
    }

    #[test]
    fn pattern_record_with_rest() {
        let (pat, diags) = parse_pat("User { name: n, .. } => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        if let Pattern::Record { fields, rest, .. } = pat {
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].name.name, "name");
            assert!(rest, "rest flag should be set for `..`");
        } else {
            panic!("expected Record pattern");
        }
    }

    #[test]
    fn pattern_tuple() {
        let (pat, diags) = parse_pat("(a, b, c) => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        if let Pattern::Tuple { elems, .. } = pat {
            assert_eq!(elems.len(), 3);
        } else {
            panic!("expected Tuple pattern");
        }
    }

    #[test]
    fn pattern_list_with_rest() {
        let (pat, diags) = parse_pat("[first, ..rest] => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        if let Pattern::List { elems, rest, .. } = pat {
            assert_eq!(elems.len(), 1);
            assert!(matches!(elems[0], Pattern::Bind { .. }));
            let r = rest.expect("rest pattern expected");
            assert!(matches!(*r, Pattern::Bind { .. }));
        } else {
            panic!("expected List pattern");
        }
    }

    #[test]
    fn pattern_list_rest_only() {
        let (pat, diags) = parse_pat("[..] => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        if let Pattern::List { elems, rest, .. } = pat {
            assert!(elems.is_empty());
            assert!(rest.is_some());
        } else {
            panic!("expected List pattern");
        }
    }

    #[test]
    fn pattern_or() {
        let (pat, diags) = parse_pat("A | B | C => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        if let Pattern::Or { alternatives, .. } = pat {
            assert_eq!(alternatives.len(), 3);
        } else {
            panic!("expected Or pattern");
        }
    }

    #[test]
    fn pattern_range() {
        let (pat, diags) = parse_pat("1..10 => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        if let Pattern::Range { inclusive, .. } = pat {
            assert!(!inclusive);
        } else {
            panic!("expected Range pattern");
        }
    }

    #[test]
    fn pattern_nested_constructor() {
        // Some(Ok((a, b)))
        let (pat, diags) = parse_pat("Some(Ok((a, b))) => 1");
        assert!(!diags.has_errors(), "{diags:?}");
        if let Pattern::Constructor { fields, .. } = pat {
            assert_eq!(fields.len(), 1, "Some should have 1 field");
            if let Pattern::Constructor { fields: inner, .. } = &fields[0] {
                assert_eq!(inner.len(), 1, "Ok should have 1 field");
                assert!(matches!(inner[0], Pattern::Tuple { .. }));
            } else {
                panic!("expected inner Constructor (Ok)");
            }
        } else {
            panic!("expected outer Constructor (Some)");
        }
    }

    #[test]
    fn pattern_or_in_match_arm() {
        let src = "fn f() { match x {\n1 | 2 => \"small\"\n_ => \"other\"\n} }";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        let arms = match fn_decl.body.as_ref().unwrap().tail.as_deref() {
            Some(Expr::Match { arms, .. }) => arms.clone(),
            _ => match fn_decl.body.as_ref().unwrap().stmts.first() {
                Some(Stmt::Expr(Expr::Match { arms, .. })) => arms.clone(),
                _ => panic!("expected match"),
            },
        };
        assert_eq!(arms.len(), 2);
        assert!(matches!(arms[0].pattern, Pattern::Or { .. }));
    }

    #[test]
    fn pattern_guard_in_match_arm() {
        let src = "fn f() { match x {\nn if (n > 100) => \"large\"\n_ => \"other\"\n} }";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        let arms = match fn_decl.body.as_ref().unwrap().tail.as_deref() {
            Some(Expr::Match { arms, .. }) => arms.clone(),
            _ => match fn_decl.body.as_ref().unwrap().stmts.first() {
                Some(Stmt::Expr(Expr::Match { arms, .. })) => arms.clone(),
                _ => panic!("expected match"),
            },
        };
        assert_eq!(arms.len(), 2);
        assert!(arms[0].guard.is_some(), "first arm should have guard");
        assert!(arms[1].guard.is_none());
    }

    #[test]
    fn pattern_full_match_example() {
        // Based on the spec example:
        // match value {
        //   0 => "zero"
        //   1 | 2 => "small"
        //   n if (n > 100) => "large"
        //   Point { x: 0, y } => "on y-axis"
        //   Some(Ok(v)) => "got it"
        //   [first, ..rest] => "head"
        //   _ => "other"
        // }
        let src = r#"fn f() {
match value {
  0 => "zero"
  1 | 2 => "small"
  n if (n > 100) => "large"
  Point { x: 0, y } => "on y-axis"
  Some(Ok(v)) => "got it"
  [first, ..rest] => "head"
  _ => "other"
}
}"#;
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        let arms = match fn_decl.body.as_ref().unwrap().tail.as_deref() {
            Some(Expr::Match { arms, .. }) => arms.clone(),
            _ => match fn_decl.body.as_ref().unwrap().stmts.first() {
                Some(Stmt::Expr(Expr::Match { arms, .. })) => arms.clone(),
                _ => panic!("expected match"),
            },
        };
        assert_eq!(arms.len(), 7);
        assert!(matches!(
            arms[0].pattern,
            Pattern::Literal {
                lit: Literal::Int(_),
                ..
            }
        ));
        assert!(matches!(arms[1].pattern, Pattern::Or { .. }));
        assert!(arms[2].guard.is_some());
        assert!(matches!(arms[3].pattern, Pattern::Record { .. }));
        assert!(matches!(arms[4].pattern, Pattern::Constructor { .. }));
        assert!(matches!(arms[5].pattern, Pattern::List { .. }));
        assert!(matches!(arms[6].pattern, Pattern::Wildcard { .. }));
    }

    #[test]
    fn pattern_if_let() {
        let src = "fn f() { if (let Some(user) = find(id)) { consume(user) } }";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let fn_decl = match m.items.first().unwrap() {
            Item::Fn(f) => f,
            _ => panic!(),
        };
        // The if-let should parse without errors — let_pattern should be Some.
        let has_if_let = fn_decl.body.as_ref().unwrap().stmts.iter().any(|s| {
            matches!(
                s,
                Stmt::Expr(Expr::If {
                    let_pattern: Some(_),
                    ..
                })
            )
        }) || fn_decl
            .body
            .as_ref()
            .unwrap()
            .tail
            .as_ref()
            .is_some_and(|t| {
                matches!(
                    t.as_ref(),
                    Expr::If {
                        let_pattern: Some(_),
                        ..
                    }
                )
            });
        assert!(has_if_let, "expected if-let expression with let_pattern");
    }

    // ── P2.8: Type expression parsing tests ──────────────────────────────────

    /// Parse a type expression from a function parameter annotation: `fn f(x: <ty>) {}`.
    fn parse_type_str(ty: &str) -> (TypeExpr, DiagnosticBag) {
        let src = format!("fn f(x: {ty}) {{}}\n");
        let (m, diags) = parse(&src);
        let Item::Fn(f) = &m.items[0] else {
            panic!("expected fn")
        };
        let ty = f.params[0].ty.clone().expect("param should have type");
        (ty, diags)
    }

    #[test]
    fn type_named_simple() {
        let (ty, diags) = parse_type_str("Int");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Named { path, args, .. } = ty else {
            panic!("expected Named")
        };
        assert_eq!(path.segments[0].name, "Int");
        assert!(args.is_empty());
    }

    #[test]
    fn type_named_module_path() {
        let (ty, diags) = parse_type_str("app.models.User");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Named { path, .. } = ty else {
            panic!("expected Named")
        };
        let names: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["app", "models", "User"]);
    }

    #[test]
    fn type_generic_single() {
        let (ty, diags) = parse_type_str("List[Int]");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Named { path, args, .. } = ty else {
            panic!("expected Named")
        };
        assert_eq!(path.segments[0].name, "List");
        assert_eq!(args.len(), 1);
        assert!(matches!(&args[0], TypeExpr::Named { path, .. } if path.segments[0].name == "Int"));
    }

    #[test]
    fn type_generic_two_params() {
        let (ty, diags) = parse_type_str("Map[String, Int]");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Named { path, args, .. } = ty else {
            panic!("expected Named")
        };
        assert_eq!(path.segments[0].name, "Map");
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn type_generic_nested() {
        let (ty, diags) = parse_type_str("Map[String, List[User]]");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Named { args, .. } = ty else {
            panic!("expected Named")
        };
        assert_eq!(args.len(), 2);
        let TypeExpr::Named {
            path: inner_path,
            args: inner_args,
            ..
        } = &args[1]
        else {
            panic!("expected inner Named")
        };
        assert_eq!(inner_path.segments[0].name, "List");
        assert_eq!(inner_args.len(), 1);
    }

    #[test]
    fn type_tuple_unit() {
        let (ty, diags) = parse_type_str("()");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Tuple { elems, .. } = ty else {
            panic!("expected Tuple")
        };
        assert!(elems.is_empty());
    }

    #[test]
    fn type_tuple_two_elems() {
        let (ty, diags) = parse_type_str("(Int, String)");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Tuple { elems, .. } = ty else {
            panic!("expected Tuple")
        };
        assert_eq!(elems.len(), 2);
    }

    #[test]
    fn type_tuple_three_elems() {
        let (ty, diags) = parse_type_str("(Int, String, Bool)");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Tuple { elems, .. } = ty else {
            panic!("expected Tuple")
        };
        assert_eq!(elems.len(), 3);
    }

    #[test]
    fn type_fn_no_params() {
        let (ty, diags) = parse_type_str("Fn() -> Void");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Function {
            params,
            ret,
            effects,
            ..
        } = ty
        else {
            panic!("expected Function")
        };
        assert!(params.is_empty());
        assert!(effects.is_empty());
        assert!(
            matches!(ret.as_ref(), TypeExpr::Named { path, .. } if path.segments[0].name == "Void")
        );
    }

    #[test]
    fn type_fn_with_params_and_return() {
        let (ty, diags) = parse_type_str("Fn(Int, Int) -> Int");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Function {
            params,
            ret,
            effects,
            ..
        } = ty
        else {
            panic!("expected Function")
        };
        assert_eq!(params.len(), 2);
        assert!(effects.is_empty());
        assert!(
            matches!(ret.as_ref(), TypeExpr::Named { path, .. } if path.segments[0].name == "Int")
        );
    }

    #[test]
    fn type_fn_with_effect_clause() {
        let (ty, diags) = parse_type_str("Fn(String) -> Void with Log");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Function {
            params, effects, ..
        } = ty
        else {
            panic!("expected Function")
        };
        assert_eq!(params.len(), 1);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].segments[0].name, "Log");
    }

    #[test]
    fn type_fn_with_multiple_effects() {
        let (ty, diags) = parse_type_str("Fn() -> Void with Log, Io");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Function { effects, .. } = ty else {
            panic!("expected Function")
        };
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[0].segments[0].name, "Log");
        assert_eq!(effects[1].segments[0].name, "Io");
    }

    #[test]
    fn type_optional_shorthand() {
        let (ty, diags) = parse_type_str("User?");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Optional { inner, .. } = ty else {
            panic!("expected Optional")
        };
        let TypeExpr::Named { path, .. } = inner.as_ref() else {
            panic!("expected Named inner")
        };
        assert_eq!(path.segments[0].name, "User");
    }

    #[test]
    fn type_optional_generic() {
        let (ty, diags) = parse_type_str("List[Int]?");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Optional { inner, .. } = ty else {
            panic!("expected Optional")
        };
        assert!(matches!(inner.as_ref(), TypeExpr::Named { .. }));
    }

    #[test]
    fn type_self_in_impl() {
        // `Self` is a valid type expression; test via record field.
        let src = "record Wrap { inner: Self }\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Record(r) = &m.items[0] else {
            panic!()
        };
        assert!(matches!(r.fields[0].ty, TypeExpr::SelfType { .. }));
    }

    #[test]
    fn type_deeply_nested_generics() {
        // Map[String, List[Result[User, String]]]
        let (ty, diags) = parse_type_str("Map[String, List[Result[User, String]]]");
        assert!(!diags.has_errors(), "{diags:?}");
        let TypeExpr::Named { path, args, .. } = ty else {
            panic!("expected Named")
        };
        assert_eq!(path.segments[0].name, "Map");
        assert_eq!(args.len(), 2);
        let TypeExpr::Named {
            path: list_path,
            args: list_args,
            ..
        } = &args[1]
        else {
            panic!("expected List")
        };
        assert_eq!(list_path.segments[0].name, "List");
        assert_eq!(list_args.len(), 1);
        let TypeExpr::Named {
            path: result_path,
            args: result_args,
            ..
        } = &list_args[0]
        else {
            panic!("expected Result")
        };
        assert_eq!(result_path.segments[0].name, "Result");
        assert_eq!(result_args.len(), 2);
    }

    // ─── Effect declaration tests ─────────────────────────────────────────────

    #[test]
    fn effect_empty_body() {
        let src = "effect Log {\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::Effect(e) = &m.items[0] else {
            panic!("expected Effect")
        };
        assert_eq!(e.name.name, "Log");
        assert!(e.operations.is_empty());
    }

    #[test]
    fn effect_with_operations() {
        let src = "effect Log {\n  fn log(level: Level, message: String) -> Void\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Effect(e) = &m.items[0] else {
            panic!("expected Effect")
        };
        assert_eq!(e.name.name, "Log");
        assert_eq!(e.operations.len(), 1);
        assert_eq!(e.operations[0].name.name, "log");
        assert_eq!(e.operations[0].params.len(), 2);
    }

    #[test]
    fn effect_multiple_operations() {
        let src = "effect Storage {\n  fn read(key: String) -> Option[Bytes]\n  fn write(key: String, val: Bytes) -> Void\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Effect(e) = &m.items[0] else {
            panic!("expected Effect")
        };
        assert_eq!(e.operations.len(), 2);
        assert_eq!(e.operations[0].name.name, "read");
        assert_eq!(e.operations[1].name.name, "write");
    }

    #[test]
    fn effect_composite() {
        let src = "effect Observable = Log + Trace + Metrics\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::Effect(e) = &m.items[0] else {
            panic!("expected Effect")
        };
        assert_eq!(e.name.name, "Observable");
        assert!(
            e.operations.is_empty(),
            "composite effects have no operations"
        );
    }

    #[test]
    fn effect_with_visibility() {
        let src = "public effect Log {\n  fn log(msg: String) -> Void\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Effect(e) = &m.items[0] else {
            panic!("expected Effect")
        };
        assert_eq!(e.visibility, Visibility::Public);
    }

    // ─── Annotation tests ─────────────────────────────────────────────────────

    #[test]
    fn annotation_no_args() {
        let src = "@deprecated\nfn old() {}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.annotations.len(), 1);
        assert_eq!(f.annotations[0].name.name, "deprecated");
        assert!(f.annotations[0].args.is_empty());
    }

    #[test]
    fn annotation_positional_arg() {
        let src = "@domain(\"e-commerce\")\nfn process() {}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.annotations[0].name.name, "domain");
        assert_eq!(f.annotations[0].args.len(), 1);
    }

    #[test]
    fn annotation_named_arg() {
        let src = "@performance(max_latency: 100)\nfn fast() {}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.annotations[0].name.name, "performance");
        assert_eq!(f.annotations[0].args.len(), 1);
    }

    #[test]
    fn annotation_multiline_string() {
        let src = "@context(\"\"\"\n  Payment module.\n\"\"\")\nfn pay() {}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.annotations[0].name.name, "context");
        assert_eq!(f.annotations[0].args.len(), 1);
    }

    #[test]
    fn multiple_annotations_stack() {
        let src = "@deprecated\n@domain(\"old\")\nfn legacy() {}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.annotations.len(), 2);
        assert_eq!(f.annotations[0].name.name, "deprecated");
        assert_eq!(f.annotations[1].name.name, "domain");
    }

    // ─── Module handle declaration tests ─────────────────────────────────────

    #[test]
    fn module_handle_decl() {
        let src = "handle Log with ConsoleLog\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::ModuleHandle(h) = &m.items[0] else {
            panic!("expected ModuleHandle")
        };
        assert_eq!(h.effect.segments[0].name, "Log");
    }

    #[test]
    fn module_handle_decl_qualified_effect() {
        let src = "handle Std.Io with FileIo\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::ModuleHandle(h) = &m.items[0] else {
            panic!("expected ModuleHandle")
        };
        assert_eq!(h.effect.segments.len(), 2);
        assert_eq!(h.effect.segments[0].name, "Std");
        assert_eq!(h.effect.segments[1].name, "Io");
    }

    // ─── Effect clause integration test ──────────────────────────────────────

    #[test]
    fn fn_with_effect_clause_integration() {
        let src = "fn process(data: Data) -> Result\n  with Log, Clock\n{\n  data\n}\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        assert_eq!(f.effect_clause.len(), 2);
        assert_eq!(f.effect_clause[0].segments[0].name, "Log");
        assert_eq!(f.effect_clause[1].segments[0].name, "Clock");
    }

    // ─── P2.10: Disambiguation audit tests ───────────────────────────────────

    /// Rule 1: `{` after TYPE_IDENT → record construction (not map or block).
    #[test]
    fn disambig_brace_after_type_ident_is_record_construct() {
        let src = "fn f() { Point { x: 1, y: 2 } }\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        let tail = f.body.as_ref().unwrap().tail.as_deref().expect("tail expr");
        assert!(
            matches!(tail, Expr::RecordConstruct { .. }),
            "expected RecordConstruct, got {tail:?}"
        );
    }

    /// Rule 2: `{` with first element `expr ':'` → map literal.
    #[test]
    fn disambig_brace_with_colon_is_map() {
        let src = "fn f() { { \"key\": 42 } }\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        let tail = f.body.as_ref().unwrap().tail.as_deref().expect("tail expr");
        assert!(
            matches!(tail, Expr::MapLiteral { .. }),
            "expected MapLiteral, got {tail:?}"
        );
    }

    /// Rule 3: `{` without colon after first element → block.
    #[test]
    fn disambig_brace_without_colon_is_block() {
        let src = "fn f() { { 42 } }\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        let tail = f.body.as_ref().unwrap().tail.as_deref().expect("tail expr");
        assert!(
            matches!(tail, Expr::Block { .. }),
            "expected Block, got {tail:?}"
        );
    }

    /// Rule 4a: `(expr)` → grouped expression (not a tuple).
    #[test]
    fn disambig_single_paren_is_grouping() {
        let src = "fn f() { (42) }\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        let tail = f.body.as_ref().unwrap().tail.as_deref().expect("tail expr");
        // Grouped expression is returned as the inner expr (not TupleLiteral).
        assert!(
            !matches!(tail, Expr::TupleLiteral { .. }),
            "should not be TupleLiteral"
        );
    }

    /// Rule 4b: `(expr, ...)` → tuple.
    #[test]
    fn disambig_multi_paren_is_tuple() {
        let src = "fn f() { (1, 2) }\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        let tail = f.body.as_ref().unwrap().tail.as_deref().expect("tail expr");
        assert!(matches!(tail, Expr::TupleLiteral { elems, .. } if elems.len() == 2));
    }

    /// Rule 4c: trailing comma `(expr,)` → single-element tuple.
    #[test]
    fn disambig_trailing_comma_is_single_elem_tuple() {
        let src = "fn f() { (1,) }\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        let tail = f.body.as_ref().unwrap().tail.as_deref().expect("tail expr");
        assert!(matches!(tail, Expr::TupleLiteral { elems, .. } if elems.len() == 1));
    }

    /// Rule: map literal with ident key (not just string keys).
    #[test]
    fn disambig_map_with_ident_key() {
        let src = "fn f() { { name: \"Alice\" } }\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        let tail = f.body.as_ref().unwrap().tail.as_deref().expect("tail expr");
        assert!(matches!(tail, Expr::MapLiteral { entries, .. } if entries.len() == 1));
    }

    /// Empty map `{}` — no colon so it's an empty block, not a map.
    #[test]
    fn disambig_empty_braces_is_block() {
        let src = "fn f() { {} }\n";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let Item::Fn(f) = &m.items[0] else { panic!() };
        let tail = f.body.as_ref().unwrap().tail.as_deref().expect("tail expr");
        assert!(
            matches!(tail, Expr::Block { .. }),
            "expected Block, got {tail:?}"
        );
    }

    // ─── P2.10: Error recovery tests ─────────────────────────────────────────

    /// Parser recovers from an unexpected token at top level and continues
    /// parsing subsequent valid declarations.
    #[test]
    fn recovery_unexpected_token_at_top_level() {
        // `???` is not a valid declaration keyword
        let src = "fn before() {}\n???\nfn after() {}\n";
        let (m, diags) = parse(src);
        assert!(diags.has_errors(), "should have errors");
        // Both valid functions should still be parsed
        let fns: Vec<_> = m
            .items
            .iter()
            .filter(|i| matches!(i, Item::Fn(_)))
            .collect();
        assert_eq!(fns.len(), 2, "both fns should be in the AST");
        // Error node should be present
        assert!(m.items.iter().any(|i| matches!(i, Item::Error { .. })));
    }

    /// Multiple errors in one file are all reported (not just the first).
    #[test]
    fn recovery_multiple_errors_reported() {
        // Each bad section is separated by a valid fn, forcing independent recoveries.
        let src = "???\nfn mid() {}\n!!!\nfn ok() {}\n";
        let (m, diags) = parse(src);
        assert!(diags.has_errors());
        assert!(
            diags.error_count() >= 2,
            "should report multiple errors, got: {diags:?}"
        );
        // Both valid fns should be recovered
        let fns: Vec<_> = m
            .items
            .iter()
            .filter(|i| matches!(i, Item::Fn(_)))
            .collect();
        assert_eq!(fns.len(), 2);
    }

    /// Parser continues after a missing `}` closing a function body.
    #[test]
    fn recovery_after_malformed_fn_body() {
        // Missing closing brace on first fn; second fn should still parse.
        let src = "fn bad( {}\nfn good() {}\n";
        let (m, diags) = parse(src);
        assert!(diags.has_errors());
        // At least `good` should appear in items
        let has_good = m.items.iter().any(|i| {
            if let Item::Fn(f) = i {
                f.name.name == "good"
            } else {
                false
            }
        });
        assert!(
            has_good,
            "fn good should be recovered; items: {:#?}",
            m.items
        );
    }

    // ─── P2.10: Integration test — complete multi-item source file ────────────

    #[test]
    fn integration_complete_source_file() {
        let src = "\
module app.core\n\
use std.io.*\n\
use std.collections.{List, Map}\n\
\n\
public record User {\n\
  name: String\n\
  age: Int\n\
}\n\
\n\
public enum Color {\n\
  Red\n\
  Green\n\
  Blue\n\
}\n\
\n\
public fn greet(user: User) -> String {\n\
  \"Hello\"\n\
}\n\
\n\
public fn add(x: Int, y: Int) -> Int {\n\
  x + y\n\
}\n\
";
        let (m, diags) = parse(src);
        assert!(!diags.has_errors(), "errors: {diags:?}");

        // Module declaration
        let path = m.path.as_ref().expect("module path");
        assert_eq!(path.segments[0].name, "app");
        assert_eq!(path.segments[1].name, "core");

        // Two imports
        assert_eq!(m.imports.len(), 2);

        // Items: record, enum, fn, fn
        assert_eq!(m.items.len(), 4);
        assert!(matches!(m.items[0], Item::Record(_)));
        assert!(matches!(m.items[1], Item::Enum(_)));
        assert!(matches!(m.items[2], Item::Fn(_)));
        assert!(matches!(m.items[3], Item::Fn(_)));

        let Item::Record(r) = &m.items[0] else {
            panic!()
        };
        assert_eq!(r.name.name, "User");
        assert_eq!(r.fields.len(), 2);

        let Item::Enum(e) = &m.items[1] else { panic!() };
        assert_eq!(e.name.name, "Color");
        assert_eq!(e.variants.len(), 3);
    }

    // ─── Type alias tests ──────────────────────────────────────────────────

    #[test]
    fn parse_type_alias_simple() {
        let (m, diags) = parse("type Email = String\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::TypeAlias(ta) = &m.items[0] else {
            panic!("expected TypeAlias")
        };
        assert_eq!(ta.name.name, "Email");
        assert!(ta.generic_params.is_empty());
        assert!(ta.where_clause.is_empty());
        assert_eq!(ta.visibility, Visibility::Private);
    }

    #[test]
    fn parse_type_alias_generic() {
        let (m, diags) = parse("type NonEmpty[T] = List[T]\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::TypeAlias(ta) = &m.items[0] else {
            panic!("expected TypeAlias")
        };
        assert_eq!(ta.name.name, "NonEmpty");
        assert_eq!(ta.generic_params.len(), 1);
        assert_eq!(ta.generic_params[0].name.name, "T");
    }

    #[test]
    fn parse_type_alias_plain() {
        let (m, diags) = parse("type Port = Int\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::TypeAlias(ta) = &m.items[0] else {
            panic!("expected TypeAlias")
        };
        assert_eq!(ta.name.name, "Port");
        assert!(ta.generic_params.is_empty());
    }

    #[test]
    fn parse_type_alias_with_where_clause() {
        let (m, diags) = parse("type Sortable[T] = List[T] where (T: Comparable)\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::TypeAlias(ta) = &m.items[0] else {
            panic!("expected TypeAlias")
        };
        assert_eq!(ta.name.name, "Sortable");
        assert_eq!(ta.generic_params.len(), 1);
        assert_eq!(ta.where_clause.len(), 1);
        assert_eq!(ta.where_clause[0].param.name, "T");
    }

    // ─── Const declaration tests ──────────────────────────────────────────

    #[test]
    fn parse_const_int() {
        let (m, diags) = parse("const MAX_SIZE: Int = 1024\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::Const(cd) = &m.items[0] else {
            panic!("expected Const")
        };
        assert_eq!(cd.name.name, "MAX_SIZE");
        assert_eq!(cd.visibility, Visibility::Private);
    }

    #[test]
    fn parse_const_float() {
        let (m, diags) = parse("const PI: Float = 3.14159\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::Const(cd) = &m.items[0] else {
            panic!("expected Const")
        };
        assert_eq!(cd.name.name, "PI");
    }

    #[test]
    fn parse_const_with_visibility() {
        let (m, diags) = parse("public const VERSION: String = \"1.0.0\"\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::Const(cd) = &m.items[0] else {
            panic!("expected Const")
        };
        assert_eq!(cd.name.name, "VERSION");
        assert_eq!(cd.visibility, Visibility::Public);
    }

    #[test]
    fn parse_type_alias_with_visibility() {
        let (m, diags) = parse("public type UserId = Int\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::TypeAlias(ta) = &m.items[0] else {
            panic!("expected TypeAlias")
        };
        assert_eq!(ta.name.name, "UserId");
        assert_eq!(ta.visibility, Visibility::Public);
    }

    // ─── F2.02: Parser stores previously discarded data ─────────────────

    #[test]
    fn import_visibility_stored() {
        let (m, diags) = parse("public use app.models.User\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.imports.len(), 1);
        assert_eq!(m.imports[0].visibility, Visibility::Public);
    }

    #[test]
    fn import_private_visibility_default() {
        let (m, diags) = parse("use app.models.User\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.imports.len(), 1);
        assert_eq!(m.imports[0].visibility, Visibility::Private);
    }

    #[test]
    fn composite_effect_components_stored() {
        let (m, diags) = parse("effect IO = Log + Clock + Storage\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::Effect(eff) = &m.items[0] else {
            panic!("expected Effect")
        };
        assert_eq!(eff.name.name, "IO");
        let component_names: Vec<&str> = eff
            .components
            .iter()
            .map(|c| c.segments[0].name.as_str())
            .collect();
        assert_eq!(component_names, ["Log", "Clock", "Storage"]);
    }

    #[test]
    fn trait_supertraits_stored() {
        let (m, diags) = parse("trait Ordered: Comparable, Equatable {\n}\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::Trait(tr) = &m.items[0] else {
            panic!("expected Trait")
        };
        assert_eq!(tr.name.name, "Ordered");
        let supertrait_names: Vec<&str> = tr
            .supertraits
            .iter()
            .map(|s| s.segments[0].name.as_str())
            .collect();
        assert_eq!(supertrait_names, ["Comparable", "Equatable"]);
    }

    #[test]
    fn import_alias_in_list() {
        let (m, diags) = parse("use json.{Value as JsonValue}\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.imports.len(), 1);
        match &m.imports[0].items {
            ImportItems::Named(names) => {
                assert_eq!(names.len(), 1);
                assert_eq!(names[0].name.name, "Value");
                assert_eq!(names[0].alias.as_ref().unwrap().name, "JsonValue");
            }
            other => panic!("expected Named import, got {other:?}"),
        }
    }

    #[test]
    fn annotation_named_args_preserve_labels() {
        let (m, diags) = parse("@performance(max_latency: 100, max_memory: 50)\nfn fast() {}\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::Fn(f) = &m.items[0] else {
            panic!("expected Fn")
        };
        assert_eq!(f.annotations.len(), 1);
        let ann = &f.annotations[0];
        assert_eq!(ann.name.name, "performance");
        assert_eq!(ann.args.len(), 2);
        assert_eq!(ann.args[0].label.as_ref().unwrap().name, "max_latency");
        assert_eq!(ann.args[1].label.as_ref().unwrap().name, "max_memory");
    }

    #[test]
    fn trait_required_method_body_is_none() {
        let (m, diags) = parse("trait Foo {\n  fn bar(self) -> Int\n}\n");
        assert!(!diags.has_errors(), "errors: {diags:?}");
        assert_eq!(m.items.len(), 1);
        let Item::Trait(tr) = &m.items[0] else {
            panic!("expected Trait")
        };
        assert_eq!(tr.methods.len(), 1);
        assert_eq!(tr.methods[0].name.name, "bar");
        assert!(
            tr.methods[0].body.is_none(),
            "required method should have body: None"
        );
    }

    // ─── Systematic precedence & associativity tests (F3.02 / M-050 + M-053) ──

    // --- M-050: Comparison operators are non-associative ---

    #[test]
    fn comparison_chained_eq_is_error() {
        // `a == b == c` must be a parse error (non-associative)
        let (_, diags) = parse_expr_str("a == b == c");
        assert!(diags.has_errors(), "chained == must produce a parse error");
    }

    #[test]
    fn comparison_chained_ne_is_error() {
        let (_, diags) = parse_expr_str("a != b != c");
        assert!(diags.has_errors(), "chained != must produce a parse error");
    }

    #[test]
    fn comparison_chained_lt_gt_is_error() {
        let (_, diags) = parse_expr_str("a < b > c");
        assert!(diags.has_errors(), "chained < > must produce a parse error");
    }

    #[test]
    fn comparison_chained_le_ge_is_error() {
        let (_, diags) = parse_expr_str("a <= b >= c");
        assert!(
            diags.has_errors(),
            "chained <= >= must produce a parse error"
        );
    }

    #[test]
    fn comparison_single_eq_still_works() {
        let (e, diags) = parse_expr_str("a == b");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Binary { op: BinOp::Eq, .. }));
    }

    #[test]
    fn comparison_single_lt_still_works() {
        let (e, diags) = parse_expr_str("a < b");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(e, Expr::Binary { op: BinOp::Lt, .. }));
    }

    // --- M-053: Precedence tests for all 15 levels ---
    // For each adjacent pair, the higher-precedence operator binds tighter.

    #[test]
    fn prec_01_02_assignment_wraps_pipe() {
        // `a = b |> c` → Assign(a, Pipe(b, c))
        let (e, diags) = parse_expr_str("a = b |> c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Assign { value, .. } => {
                assert!(
                    matches!(value.as_ref(), Expr::Pipe { .. }),
                    "assignment RHS should be Pipe, got {value:?}"
                );
            }
            _ => panic!("expected Assign, got {e:?}"),
        }
    }

    #[test]
    fn prec_02_03_pipe_wraps_compose() {
        // `a |> b >> c` → Pipe(a, Compose(b, c))
        let (e, diags) = parse_expr_str("a |> b >> c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Pipe { right, .. } => {
                assert!(
                    matches!(right.as_ref(), Expr::Compose { .. }),
                    "pipe RHS should be Compose, got {right:?}"
                );
            }
            _ => panic!("expected Pipe, got {e:?}"),
        }
    }

    #[test]
    fn prec_03_04_compose_wraps_range() {
        // `a >> b .. c` → Compose(a, Range(b, c))
        let (e, diags) = parse_expr_str("a >> b .. c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Compose { right, .. } => {
                assert!(
                    matches!(right.as_ref(), Expr::Range { .. }),
                    "compose RHS should be Range, got {right:?}"
                );
            }
            _ => panic!("expected Compose, got {e:?}"),
        }
    }

    #[test]
    fn prec_04_05_range_wraps_logical_or() {
        // `a .. b || c` → Range(a, Or(b, c))
        let (e, diags) = parse_expr_str("a .. b || c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Range { hi, .. } => {
                assert!(
                    matches!(hi.as_ref(), Expr::Binary { op: BinOp::Or, .. }),
                    "range hi should be Or, got {hi:?}"
                );
            }
            _ => panic!("expected Range, got {e:?}"),
        }
    }

    #[test]
    fn prec_05_06_or_wraps_and() {
        // `a || b && c` → Or(a, And(b, c))
        let (e, diags) = parse_expr_str("a || b && c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::Or,
                right,
                ..
            } => {
                assert!(
                    matches!(right.as_ref(), Expr::Binary { op: BinOp::And, .. }),
                    "Or RHS should be And, got {right:?}"
                );
            }
            _ => panic!("expected Or, got {e:?}"),
        }
    }

    #[test]
    fn prec_06_07_and_wraps_comparison() {
        // `a && b == c` → And(a, Eq(b, c))
        let (e, diags) = parse_expr_str("a && b == c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::And,
                right,
                ..
            } => {
                assert!(
                    matches!(right.as_ref(), Expr::Binary { op: BinOp::Eq, .. }),
                    "And RHS should be Eq, got {right:?}"
                );
            }
            _ => panic!("expected And, got {e:?}"),
        }
    }

    #[test]
    fn prec_07_08_comparison_wraps_bitor() {
        // `a == b | c` → Eq(a, BitOr(b, c))
        let (e, diags) = parse_expr_str("a == b | c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::Eq,
                right,
                ..
            } => {
                assert!(
                    matches!(
                        right.as_ref(),
                        Expr::Binary {
                            op: BinOp::BitOr,
                            ..
                        }
                    ),
                    "Eq RHS should be BitOr, got {right:?}"
                );
            }
            _ => panic!("expected Eq, got {e:?}"),
        }
    }

    #[test]
    fn prec_08_09_bitor_wraps_bitxor() {
        // `a | b ^ c` → BitOr(a, BitXor(b, c))
        let (e, diags) = parse_expr_str("a | b ^ c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::BitOr,
                right,
                ..
            } => {
                assert!(
                    matches!(
                        right.as_ref(),
                        Expr::Binary {
                            op: BinOp::BitXor,
                            ..
                        }
                    ),
                    "BitOr RHS should be BitXor, got {right:?}"
                );
            }
            _ => panic!("expected BitOr, got {e:?}"),
        }
    }

    #[test]
    fn prec_09_10_bitxor_wraps_bitand() {
        // `a ^ b & c` → BitXor(a, BitAnd(b, c))
        let (e, diags) = parse_expr_str("a ^ b & c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::BitXor,
                right,
                ..
            } => {
                assert!(
                    matches!(
                        right.as_ref(),
                        Expr::Binary {
                            op: BinOp::BitAnd,
                            ..
                        }
                    ),
                    "BitXor RHS should be BitAnd, got {right:?}"
                );
            }
            _ => panic!("expected BitXor, got {e:?}"),
        }
    }

    #[test]
    fn prec_10_11_bitand_wraps_add() {
        // `a & b + c` → BitAnd(a, Add(b, c))
        let (e, diags) = parse_expr_str("a & b + c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::BitAnd,
                right,
                ..
            } => {
                assert!(
                    matches!(right.as_ref(), Expr::Binary { op: BinOp::Add, .. }),
                    "BitAnd RHS should be Add, got {right:?}"
                );
            }
            _ => panic!("expected BitAnd, got {e:?}"),
        }
    }

    #[test]
    fn prec_11_12_add_wraps_mul() {
        // `a + b * c` → Add(a, Mul(b, c))
        let (e, diags) = parse_expr_str("a + b * c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::Add,
                right,
                ..
            } => {
                assert!(
                    matches!(right.as_ref(), Expr::Binary { op: BinOp::Mul, .. }),
                    "Add RHS should be Mul, got {right:?}"
                );
            }
            _ => panic!("expected Add, got {e:?}"),
        }
    }

    #[test]
    fn prec_12_13_mul_wraps_power() {
        // `a * b ** c` → Mul(a, Pow(b, c))
        let (e, diags) = parse_expr_str("a * b ** c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::Mul,
                right,
                ..
            } => {
                assert!(
                    matches!(right.as_ref(), Expr::Binary { op: BinOp::Pow, .. }),
                    "Mul RHS should be Pow, got {right:?}"
                );
            }
            _ => panic!("expected Mul, got {e:?}"),
        }
    }

    #[test]
    fn prec_13_14_power_wraps_unary() {
        // `-a ** b` → Pow(Neg(a), b) — unary binds tighter than power
        let (e, diags) = parse_expr_str("-a ** b");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::Pow,
                left,
                ..
            } => {
                assert!(
                    matches!(
                        left.as_ref(),
                        Expr::Unary {
                            op: UnaryOp::Neg,
                            ..
                        }
                    ),
                    "Pow LHS should be Neg, got {left:?}"
                );
            }
            _ => panic!("expected Pow, got {e:?}"),
        }
    }

    #[test]
    fn prec_14_15_unary_wraps_postfix() {
        // `!a.b` → Not(FieldAccess(a, b)) — postfix binds tighter than unary
        let (e, diags) = parse_expr_str("!a.b");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Unary {
                op: UnaryOp::Not,
                operand,
                ..
            } => {
                assert!(
                    matches!(operand.as_ref(), Expr::FieldAccess { .. }),
                    "Not operand should be FieldAccess, got {operand:?}"
                );
            }
            _ => panic!("expected Unary(Not), got {e:?}"),
        }
    }

    // --- Associativity tests for each level ---

    #[test]
    fn assoc_01_assignment_right() {
        // `a = b = c` → Assign(a, Assign(b, c)) — right-associative
        let (e, diags) = parse_expr_str("a = b = c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Assign { value, .. } => {
                assert!(
                    matches!(value.as_ref(), Expr::Assign { .. }),
                    "assignment should be right-assoc, got {value:?}"
                );
            }
            _ => panic!("expected Assign, got {e:?}"),
        }
    }

    #[test]
    fn assoc_02_pipe_left() {
        // `a |> b |> c` → Pipe(Pipe(a, b), c) — left-associative
        let (e, diags) = parse_expr_str("a |> b |> c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Pipe { left, .. } => {
                assert!(
                    matches!(left.as_ref(), Expr::Pipe { .. }),
                    "pipe should be left-assoc, got {left:?}"
                );
            }
            _ => panic!("expected Pipe, got {e:?}"),
        }
    }

    #[test]
    fn assoc_03_compose_left() {
        // `a >> b >> c` → Compose(Compose(a, b), c) — left-associative
        let (e, diags) = parse_expr_str("a >> b >> c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Compose { left, .. } => {
                assert!(
                    matches!(left.as_ref(), Expr::Compose { .. }),
                    "compose should be left-assoc, got {left:?}"
                );
            }
            _ => panic!("expected Compose, got {e:?}"),
        }
    }

    #[test]
    fn assoc_04_range_non_assoc() {
        // `a .. b .. c` must be a parse error (non-associative)
        let (_, diags) = parse_expr_str("a .. b .. c");
        assert!(diags.has_errors(), "chained .. must produce a parse error");
    }

    #[test]
    fn assoc_05_or_left() {
        // `a || b || c` → Or(Or(a, b), c) — left-associative
        let (e, diags) = parse_expr_str("a || b || c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::Or,
                left,
                ..
            } => {
                assert!(
                    matches!(left.as_ref(), Expr::Binary { op: BinOp::Or, .. }),
                    "|| should be left-assoc, got {left:?}"
                );
            }
            _ => panic!("expected Or, got {e:?}"),
        }
    }

    #[test]
    fn assoc_06_and_left() {
        // `a && b && c` → And(And(a, b), c) — left-associative
        let (e, diags) = parse_expr_str("a && b && c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::And,
                left,
                ..
            } => {
                assert!(
                    matches!(left.as_ref(), Expr::Binary { op: BinOp::And, .. }),
                    "&& should be left-assoc, got {left:?}"
                );
            }
            _ => panic!("expected And, got {e:?}"),
        }
    }

    #[test]
    fn assoc_07_comparison_non_assoc() {
        // `a == b == c` must be a parse error (non-associative) — same as M-050 test
        let (_, diags) = parse_expr_str("a == b == c");
        assert!(diags.has_errors(), "chained == must produce a parse error");
    }

    #[test]
    fn assoc_08_bitor_left() {
        // `a | b | c` → BitOr(BitOr(a, b), c) — left-associative
        let (e, diags) = parse_expr_str("a | b | c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::BitOr,
                left,
                ..
            } => {
                assert!(
                    matches!(
                        left.as_ref(),
                        Expr::Binary {
                            op: BinOp::BitOr,
                            ..
                        }
                    ),
                    "| should be left-assoc, got {left:?}"
                );
            }
            _ => panic!("expected BitOr, got {e:?}"),
        }
    }

    #[test]
    fn assoc_09_bitxor_left() {
        // `a ^ b ^ c` → BitXor(BitXor(a, b), c) — left-associative
        let (e, diags) = parse_expr_str("a ^ b ^ c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::BitXor,
                left,
                ..
            } => {
                assert!(
                    matches!(
                        left.as_ref(),
                        Expr::Binary {
                            op: BinOp::BitXor,
                            ..
                        }
                    ),
                    "^ should be left-assoc, got {left:?}"
                );
            }
            _ => panic!("expected BitXor, got {e:?}"),
        }
    }

    #[test]
    fn assoc_10_bitand_left() {
        // `a & b & c` → BitAnd(BitAnd(a, b), c) — left-associative
        let (e, diags) = parse_expr_str("a & b & c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::BitAnd,
                left,
                ..
            } => {
                assert!(
                    matches!(
                        left.as_ref(),
                        Expr::Binary {
                            op: BinOp::BitAnd,
                            ..
                        }
                    ),
                    "& should be left-assoc, got {left:?}"
                );
            }
            _ => panic!("expected BitAnd, got {e:?}"),
        }
    }

    #[test]
    fn assoc_11_add_left() {
        // `a - b - c` → Sub(Sub(a, b), c) — left-associative
        let (e, diags) = parse_expr_str("a - b - c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::Sub,
                left,
                ..
            } => {
                assert!(
                    matches!(left.as_ref(), Expr::Binary { op: BinOp::Sub, .. }),
                    "- should be left-assoc, got {left:?}"
                );
            }
            _ => panic!("expected Sub, got {e:?}"),
        }
    }

    #[test]
    fn assoc_12_mul_left() {
        // `a / b / c` → Div(Div(a, b), c) — left-associative
        let (e, diags) = parse_expr_str("a / b / c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::Div,
                left,
                ..
            } => {
                assert!(
                    matches!(left.as_ref(), Expr::Binary { op: BinOp::Div, .. }),
                    "/ should be left-assoc, got {left:?}"
                );
            }
            _ => panic!("expected Div, got {e:?}"),
        }
    }

    #[test]
    fn assoc_13_power_right() {
        // `a ** b ** c` → Pow(a, Pow(b, c)) — right-associative
        let (e, diags) = parse_expr_str("a ** b ** c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Binary {
                op: BinOp::Pow,
                right,
                ..
            } => {
                assert!(
                    matches!(right.as_ref(), Expr::Binary { op: BinOp::Pow, .. }),
                    "** should be right-assoc, got {right:?}"
                );
            }
            _ => panic!("expected Pow, got {e:?}"),
        }
    }

    #[test]
    fn assoc_14_unary_chains() {
        // `--a` → Neg(Neg(a)) — unary prefix naturally chains right
        let (e, diags) = parse_expr_str("--a");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::Unary {
                op: UnaryOp::Neg,
                operand,
                ..
            } => {
                assert!(
                    matches!(
                        operand.as_ref(),
                        Expr::Unary {
                            op: UnaryOp::Neg,
                            ..
                        }
                    ),
                    "unary should chain, got {operand:?}"
                );
            }
            _ => panic!("expected Neg(Neg), got {e:?}"),
        }
    }

    #[test]
    fn assoc_15_postfix_chains_left() {
        // `a.b.c` → FieldAccess(FieldAccess(a, b), c) — postfix chains left
        let (e, diags) = parse_expr_str("a.b.c");
        assert!(!diags.has_errors(), "{diags:?}");
        match &e {
            Expr::FieldAccess { field, object, .. } => {
                assert_eq!(field.name, "c");
                assert!(
                    matches!(object.as_ref(), Expr::FieldAccess { .. }),
                    "postfix should chain left, got {object:?}"
                );
            }
            _ => panic!("expected FieldAccess, got {e:?}"),
        }
    }

    // ── F3.03: Module-qualified record construction (M-052) ──────────────

    #[test]
    fn module_qualified_record_construct() {
        // `Mod.Type { field: val }` should parse as RecordConstruct with two-segment path
        let (e, diags) = parse_expr_str("Mod.Type { field: val }");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::RecordConstruct { path, fields, .. } => {
                assert_eq!(path.segments.len(), 2);
                assert_eq!(path.segments[0].name, "Mod");
                assert_eq!(path.segments[1].name, "Type");
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].name.name, "field");
            }
            _ => panic!("expected RecordConstruct, got {e:?}"),
        }
    }

    #[test]
    fn deeply_qualified_record_construct() {
        // `A.B.C { x: 1 }` — three-segment path
        let (e, diags) = parse_expr_str("A.B.C { x: 1 }");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::RecordConstruct { path, fields, .. } => {
                assert_eq!(path.segments.len(), 3);
                assert_eq!(path.segments[0].name, "A");
                assert_eq!(path.segments[1].name, "B");
                assert_eq!(path.segments[2].name, "C");
                assert_eq!(fields.len(), 1);
            }
            _ => panic!("expected RecordConstruct, got {e:?}"),
        }
    }

    #[test]
    fn lowercase_field_access_not_record() {
        // `Mod.field { ... }` — `field` is lowercase, so this should NOT
        // be record construction (it's a field access followed by a block).
        let (e, diags) = parse_expr_str("Mod.field");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(
            matches!(e, Expr::FieldAccess { .. }),
            "expected FieldAccess, got {e:?}"
        );
    }

    // ── F3.03: Method-level type arguments (M-051) ──────────────────────

    #[test]
    fn method_type_args_single() {
        // `obj.method[T]()` should parse as MethodCall with type_args
        let (e, diags) = parse_expr_str("obj.method[T]()");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::MethodCall {
                method,
                type_args,
                args,
                ..
            } => {
                assert_eq!(method.name, "method");
                assert_eq!(type_args.len(), 1);
                assert!(args.is_empty());
            }
            _ => panic!("expected MethodCall, got {e:?}"),
        }
    }

    #[test]
    fn method_type_args_multiple() {
        // `obj.convert[From, To](x)` — multiple type args
        let (e, diags) = parse_expr_str("obj.convert[From, To](x)");
        assert!(!diags.has_errors(), "{diags:?}");
        match e {
            Expr::MethodCall {
                method,
                type_args,
                args,
                ..
            } => {
                assert_eq!(method.name, "convert");
                assert_eq!(type_args.len(), 2);
                assert_eq!(args.len(), 1);
            }
            _ => panic!("expected MethodCall, got {e:?}"),
        }
    }

    #[test]
    fn index_access_not_type_args() {
        // `obj.data[0]` — numeric index, should remain as index access
        let (e, diags) = parse_expr_str("obj.data[0]");
        assert!(!diags.has_errors(), "{diags:?}");
        // Should be Index(FieldAccess(obj, data), 0)
        assert!(matches!(e, Expr::Index { .. }), "expected Index, got {e:?}");
    }

    // ─── Doc comment tests (F0.04) ──────────────────────────────────────────

    #[test]
    fn doc_comment_before_fn_no_error() {
        let (m, diags) = parse("/// Adds two numbers\nfn add(a: Int, b: Int) -> Int { a + b }\n");
        assert!(
            !diags.has_errors(),
            "doc comment before fn caused errors: {diags:?}"
        );
        assert_eq!(m.items.len(), 1);
        assert!(matches!(m.items[0], Item::Fn(_)));
    }

    #[test]
    fn doc_comment_before_record_no_error() {
        let (m, diags) = parse("/// A user\nrecord User {\n  name: String\n}\n");
        assert!(
            !diags.has_errors(),
            "doc comment before record caused errors: {diags:?}"
        );
        assert_eq!(m.items.len(), 1);
        assert!(matches!(m.items[0], Item::Record(_)));
    }

    #[test]
    fn doc_comment_before_enum_no_error() {
        let (m, diags) = parse("/// Colors\nenum Color {\n  Red\n  Green\n}\n");
        assert!(
            !diags.has_errors(),
            "doc comment before enum caused errors: {diags:?}"
        );
        assert_eq!(m.items.len(), 1);
        assert!(matches!(m.items[0], Item::Enum(_)));
    }

    #[test]
    fn doc_comment_before_trait_no_error() {
        let (m, diags) =
            parse("/// Greetable things\ntrait Greetable {\n  fn greet() -> String\n}\n");
        assert!(
            !diags.has_errors(),
            "doc comment before trait caused errors: {diags:?}"
        );
        assert_eq!(m.items.len(), 1);
        assert!(matches!(m.items[0], Item::Trait(_)));
    }

    #[test]
    fn multiple_consecutive_doc_comments() {
        let (m, diags) = parse("/// Line 1\n/// Line 2\n/// Line 3\nfn foo() {}\n");
        assert!(
            !diags.has_errors(),
            "multiple doc comments caused errors: {diags:?}"
        );
        assert_eq!(m.items.len(), 1);
        assert!(matches!(m.items[0], Item::Fn(_)));
    }

    #[test]
    fn module_doc_comment_no_error() {
        let (m, diags) = parse("//! Module docs\n\nfn foo() {}\n");
        assert!(
            !diags.has_errors(),
            "module doc comment caused errors: {diags:?}"
        );
        assert_eq!(m.doc.len(), 1);
        assert_eq!(m.doc[0], "Module docs");
    }

    #[test]
    fn module_doc_comment_after_module_decl() {
        // FC-26: //! after `module` declaration should be valid.
        let (m, diags) = parse("module foo\n\n//! After module\n//! More docs\n\nfn bar() {}\n");
        assert!(
            !diags.has_errors(),
            "//! after module decl caused errors: {diags:?}"
        );
        assert_eq!(m.doc.len(), 2);
        assert_eq!(m.doc[0], "After module");
        assert_eq!(m.doc[1], "More docs");
        assert_eq!(m.items.len(), 1);
    }

    #[test]
    fn module_doc_comment_before_and_after_module_decl() {
        // FC-26: //! in both positions should merge into one doc list.
        let (m, diags) = parse("//! Before\nmodule foo\n//! After\nfn bar() {}\n");
        assert!(
            !diags.has_errors(),
            "//! before+after module decl caused errors: {diags:?}"
        );
        assert_eq!(m.doc.len(), 2);
        assert_eq!(m.doc[0], "Before");
        assert_eq!(m.doc[1], "After");
    }

    #[test]
    fn module_doc_comment_after_module_with_use_no_hang() {
        // FC-26: //! after module + use on next line must not hang.
        let (m, diags) = parse("module foo\n//! Docs\nuse bar.{baz}\nfn f() {}\n");
        assert!(
            !diags.has_errors(),
            "//! after module with use caused errors: {diags:?}"
        );
        assert_eq!(m.doc.len(), 1);
        assert_eq!(m.imports.len(), 1);
    }

    #[test]
    fn doc_comment_trailing_no_error() {
        // Doc comment at end of file with no following item
        let (_m, diags) = parse("/// orphan doc\n");
        assert!(
            !diags.has_errors(),
            "trailing doc comment caused errors: {diags:?}"
        );
    }
}
