//! AST-to-source emitter with canonical formatting.

use bock_ast::{
    Annotation, Arg, AssignOp, BinOp, Block, ClassDecl, ConstDecl, EffectDecl, EnumDecl,
    EnumVariant, Expr, FnDecl, ForLoop, GenericParam, GuardStmt, HandlingBlock, ImplBlock,
    ImportDecl, ImportItems, ImportedName, InterpolationPart, Item, LetStmt, Literal, LoopStmt,
    MatchArm, Module, ModuleHandleDecl, ModulePath, Param, Pattern, PropertyTestDecl, RecordDecl,
    RecordDeclField, Stmt, TraitDecl, TypeAliasDecl, TypeConstraint, TypeExpr, TypePath, UnaryOp,
    Visibility, WhileLoop,
};

use crate::comments::{Comment, CommentKind};

/// Soft line-length limit (prefer to stay within).
const SOFT_LIMIT: usize = 80;

/// Hard line-length limit (must not exceed).
const HARD_LIMIT: usize = 100;

/// Number of spaces per indentation level.
const INDENT_WIDTH: usize = 2;

/// The canonical formatter. Walks the AST and emits formatted source text.
pub struct Formatter<'a> {
    /// Output buffer.
    buf: String,
    /// Current indentation level (number of levels, not spaces).
    indent: usize,
    /// All comments extracted from the original source.
    comments: &'a [Comment],
    /// Index of the next comment to potentially emit.
    next_comment: usize,
    /// Original source text (for comment position mapping).
    source: &'a str,
}

impl<'a> Formatter<'a> {
    /// Create a new formatter.
    #[must_use]
    pub fn new(comments: &'a [Comment], source: &'a str) -> Self {
        Self {
            buf: String::with_capacity(source.len()),
            indent: 0,
            comments,
            next_comment: 0,
            source,
        }
    }

    /// Consume the formatter and return the formatted output.
    #[must_use]
    pub fn finish(mut self) -> String {
        // Emit any trailing comments
        self.emit_remaining_comments();
        // Ensure file ends with a single newline
        let trimmed = self.buf.trim_end().to_string();
        if trimmed.is_empty() {
            return String::new();
        }
        self.buf = trimmed;
        self.buf.push('\n');
        // Enforce hard line-length limit
        wrap_long_lines(&self.buf)
    }

    // ─── Indentation helpers ──────────────────────────────────────────────

    fn indent_str(&self) -> String {
        " ".repeat(self.indent * INDENT_WIDTH)
    }

    fn push_indent(&mut self) {
        let indent = self.indent_str();
        self.buf.push_str(&indent);
    }

    fn inc_indent(&mut self) {
        self.indent += 1;
    }

    fn dec_indent(&mut self) {
        self.indent = self.indent.saturating_sub(1);
    }

    // ─── Output helpers ──────────────────────────────────────────────────

    fn push(&mut self, s: &str) {
        self.buf.push_str(s);
    }

    fn push_char(&mut self, c: char) {
        self.buf.push(c);
    }

    fn newline(&mut self) {
        self.buf.push('\n');
    }

    fn push_line(&mut self, s: &str) {
        self.push_indent();
        self.push(s);
        self.newline();
    }

    /// Estimate the length of a formatted expression (for line-length decisions).
    fn estimate_expr_len(&self, expr: &Expr) -> usize {
        let mut f = Formatter::new(&[], "");
        f.format_expr(expr);
        f.buf.len()
    }

    /// Estimate the length of a formatted type expression.
    fn estimate_type_len(&self, ty: &TypeExpr) -> usize {
        let mut f = Formatter::new(&[], "");
        f.format_type_expr(ty);
        f.buf.len()
    }

    /// Check whether a function signature fits on one line.
    fn sig_fits_one_line(&self, decl: &FnDecl) -> bool {
        let mut est = 0;
        // "fn name("
        est += 3 + decl.name.name.len() + 1;
        for (i, p) in decl.params.iter().enumerate() {
            if i > 0 {
                est += 2; // ", "
            }
            est += self.estimate_param_len(p);
        }
        est += 1; // ")"
        if let Some(ret) = &decl.return_type {
            est += 4; // " -> "
            est += self.estimate_type_len(ret);
        }
        est += 2; // " {"
        self.indent * INDENT_WIDTH + est <= SOFT_LIMIT
    }

    fn estimate_param_len(&self, p: &Param) -> usize {
        let mut est = self.estimate_pattern_len(&p.pattern);
        if let Some(ty) = &p.ty {
            est += 2; // ": "
            est += self.estimate_type_len(ty);
        }
        if let Some(def) = &p.default {
            est += 3; // " = "
            est += self.estimate_expr_len(def);
        }
        est
    }

    fn estimate_pattern_len(&self, pat: &Pattern) -> usize {
        let mut f = Formatter::new(&[], "");
        f.format_pattern(pat);
        f.buf.len()
    }

    // ─── Comment emission ──────────────────────────────────────────────────

    /// Emit any line or block comments that appear before `byte_offset` in
    /// the original source.
    fn emit_comments_before(&mut self, byte_offset: usize) {
        while self.next_comment < self.comments.len() {
            let c = &self.comments[self.next_comment];
            if c.start >= byte_offset {
                break;
            }
            // Skip doc comments — they're handled by the AST
            if c.kind == CommentKind::Doc || c.kind == CommentKind::ModuleDoc {
                self.next_comment += 1;
                continue;
            }

            // Determine if this is an inline comment (on the same line as code before it)
            let is_inline = c.start > 0 && {
                let before = &self.source[..c.start];
                let last_nl = before.rfind('\n').map_or(0, |p| p + 1);
                let line_before = before[last_nl..].trim();
                !line_before.is_empty()
            };

            if is_inline {
                // Inline comment: append to current line
                // Trim any trailing whitespace from buffer, add two spaces
                let trimmed_end = self.buf.trim_end_matches(' ').len();
                self.buf.truncate(trimmed_end);
                self.push("  ");
                self.push(&c.text);
            } else {
                // Standalone comment: emit on its own line
                self.push_indent();
                self.push(&c.text);
                self.newline();
            }
            self.next_comment += 1;
        }
    }

    /// Emit any remaining comments at the end of the file.
    fn emit_remaining_comments(&mut self) {
        while self.next_comment < self.comments.len() {
            let c = &self.comments[self.next_comment];
            if c.kind == CommentKind::Doc || c.kind == CommentKind::ModuleDoc {
                self.next_comment += 1;
                continue;
            }
            self.newline();
            self.push_indent();
            self.push(&c.text);
            self.next_comment += 1;
        }
    }

    // ─── Module ───────────────────────────────────────────────────────────

    /// Format a complete module.
    pub fn format_module(&mut self, module: &Module) {
        // Module doc comments
        for doc in &module.doc {
            self.push("//! ");
            self.push(doc);
            self.newline();
        }
        if !module.doc.is_empty() {
            self.newline();
        }

        // Module path
        if let Some(path) = &module.path {
            self.push("module ");
            self.format_module_path(path);
            self.newline();
            self.newline();
        }

        // Imports (sorted)
        if !module.imports.is_empty() {
            let mut sorted_imports = module.imports.clone();
            sorted_imports.sort_by(|a, b| {
                let cat_a = import_category(a);
                let cat_b = import_category(b);
                cat_a
                    .cmp(&cat_b)
                    .then_with(|| import_path_str(a).cmp(&import_path_str(b)))
            });

            // Group by category with blank lines between groups
            let mut prev_cat = None;
            for import in &sorted_imports {
                let cat = import_category(import);
                if let Some(prev) = prev_cat {
                    if prev != cat {
                        self.newline();
                    }
                }
                self.emit_comments_before(import.span.start);
                self.format_import(import);
                prev_cat = Some(cat);
            }
            self.newline();
        }

        // Items
        let mut first = true;
        for item in &module.items {
            if !first {
                self.newline();
            }
            self.emit_comments_before(item.span().start);
            self.format_item(item);
            first = false;
        }
    }

    // ─── Imports ──────────────────────────────────────────────────────────

    fn format_import(&mut self, import: &ImportDecl) {
        self.push("use ");
        self.format_module_path(&import.path);
        match &import.items {
            ImportItems::Module => {}
            ImportItems::Glob => {
                self.push(".*");
            }
            ImportItems::Named(names) => {
                self.push(".{ ");
                for (i, name) in names.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.format_imported_name(name);
                }
                self.push(" }");
            }
        }
        self.newline();
    }

    fn format_imported_name(&mut self, name: &ImportedName) {
        self.push(&name.name.name);
        if let Some(alias) = &name.alias {
            self.push(" as ");
            self.push(&alias.name);
        }
    }

    fn format_module_path(&mut self, path: &ModulePath) {
        for (i, seg) in path.segments.iter().enumerate() {
            if i > 0 {
                self.push_char('.');
            }
            self.push(&seg.name);
        }
    }

    // ─── Items ────────────────────────────────────────────────────────────

    fn format_item(&mut self, item: &Item) {
        match item {
            Item::Fn(decl) => self.format_fn_decl(decl),
            Item::Record(decl) => self.format_record_decl(decl),
            Item::Enum(decl) => self.format_enum_decl(decl),
            Item::Class(decl) => self.format_class_decl(decl),
            Item::Trait(decl) | Item::PlatformTrait(decl) => {
                self.format_trait_decl(decl);
            }
            Item::Impl(decl) => self.format_impl_block(decl),
            Item::Effect(decl) => self.format_effect_decl(decl),
            Item::TypeAlias(decl) => self.format_type_alias(decl),
            Item::Const(decl) => self.format_const_decl(decl),
            Item::ModuleHandle(decl) => self.format_module_handle(decl),
            Item::PropertyTest(decl) => self.format_property_test(decl),
            Item::Error { .. } => {}
        }
    }

    // ─── Annotations ──────────────────────────────────────────────────────

    fn format_annotations(&mut self, annotations: &[Annotation]) {
        for ann in annotations {
            self.push_indent();
            self.push_char('@');
            self.push(&ann.name.name);
            if !ann.args.is_empty() {
                self.push_char('(');
                for (i, arg) in ann.args.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    if let Some(label) = &arg.label {
                        self.push(&label.name);
                        self.push(": ");
                    }
                    self.format_expr(&arg.value);
                }
                self.push_char(')');
            }
            self.newline();
        }
    }

    // ─── Visibility ───────────────────────────────────────────────────────

    fn format_visibility(&mut self, vis: Visibility) {
        match vis {
            Visibility::Private => {}
            Visibility::Internal => self.push("internal "),
            Visibility::Public => self.push("pub "),
        }
    }

    // ─── Functions ────────────────────────────────────────────────────────

    fn format_fn_decl(&mut self, decl: &FnDecl) {
        // Doc comments from AST (already in annotations? no — in the AST as Module.doc)
        // Doc comments for items are tracked via span-based comment emission
        self.format_annotations(&decl.annotations);
        self.push_indent();
        self.format_visibility(decl.visibility);
        if decl.is_async {
            self.push("async ");
        }
        self.push("fn ");
        self.push(&decl.name.name);

        // Generic params
        self.format_generic_params(&decl.generic_params);

        if self.sig_fits_one_line(decl) {
            // Single-line signature
            self.push_char('(');
            for (i, p) in decl.params.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.format_param(p);
            }
            self.push_char(')');
        } else {
            // Multi-line: one param per line
            self.push_char('(');
            self.newline();
            self.inc_indent();
            for (i, p) in decl.params.iter().enumerate() {
                self.push_indent();
                self.format_param(p);
                if i < decl.params.len() - 1 {
                    self.push_char(',');
                } else {
                    self.push_char(','); // trailing comma
                }
                self.newline();
            }
            self.dec_indent();
            self.push_indent();
            self.push_char(')');
        }

        // Return type
        if let Some(ret) = &decl.return_type {
            self.push(" -> ");
            self.format_type_expr(ret);
        }

        // Effect clause
        if !decl.effect_clause.is_empty() {
            self.push(" with ");
            for (i, eff) in decl.effect_clause.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.format_type_path(eff);
            }
        }

        // Where clause
        if !decl.where_clause.is_empty() {
            self.newline();
            self.inc_indent();
            self.push_indent();
            self.push("where ");
            for (i, c) in decl.where_clause.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.format_type_constraint(c);
            }
            self.dec_indent();
        }

        if let Some(body) = &decl.body {
            self.push(" ");
            self.format_block(body);
        }
        self.newline();
    }

    fn format_param(&mut self, param: &Param) {
        self.format_pattern(&param.pattern);
        if let Some(ty) = &param.ty {
            self.push(": ");
            self.format_type_expr(ty);
        }
        if let Some(def) = &param.default {
            self.push(" = ");
            self.format_expr(def);
        }
    }

    fn format_generic_params(&mut self, params: &[GenericParam]) {
        if params.is_empty() {
            return;
        }
        self.push_char('[');
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.push(&p.name.name);
            if !p.bounds.is_empty() {
                self.push(": ");
                for (j, b) in p.bounds.iter().enumerate() {
                    if j > 0 {
                        self.push(" + ");
                    }
                    self.format_type_path(b);
                }
            }
        }
        self.push_char(']');
    }

    fn format_type_constraint(&mut self, c: &TypeConstraint) {
        self.push(&c.param.name);
        self.push(": ");
        for (i, b) in c.bounds.iter().enumerate() {
            if i > 0 {
                self.push(" + ");
            }
            self.format_type_path(b);
        }
    }

    // ─── Records ──────────────────────────────────────────────────────────

    fn format_record_decl(&mut self, decl: &RecordDecl) {
        self.format_annotations(&decl.annotations);
        self.push_indent();
        self.format_visibility(decl.visibility);
        self.push("record ");
        self.push(&decl.name.name);
        self.format_generic_params(&decl.generic_params);
        self.push(" {");
        self.newline();
        self.inc_indent();
        for field in &decl.fields {
            self.emit_comments_before(field.span.start);
            self.format_record_field_decl(field);
        }
        self.dec_indent();
        self.push_line("}");
    }

    fn format_record_field_decl(&mut self, field: &RecordDeclField) {
        self.push_indent();
        self.push(&field.name.name);
        self.push(": ");
        self.format_type_expr(&field.ty);
        if let Some(def) = &field.default {
            self.push(" = ");
            self.format_expr(def);
        }
        self.push_char(',');
        self.newline();
    }

    // ─── Enums ────────────────────────────────────────────────────────────

    fn format_enum_decl(&mut self, decl: &EnumDecl) {
        self.format_annotations(&decl.annotations);
        self.push_indent();
        self.format_visibility(decl.visibility);
        self.push("enum ");
        self.push(&decl.name.name);
        self.format_generic_params(&decl.generic_params);
        self.push(" {");
        self.newline();
        self.inc_indent();
        for variant in &decl.variants {
            self.format_enum_variant(variant);
        }
        self.dec_indent();
        self.push_line("}");
    }

    fn format_enum_variant(&mut self, variant: &EnumVariant) {
        self.push_indent();
        match variant {
            EnumVariant::Unit { name, .. } => {
                self.push(&name.name);
                self.push_char(',');
            }
            EnumVariant::Tuple { name, tys, .. } => {
                self.push(&name.name);
                self.push_char('(');
                for (i, ty) in tys.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.format_type_expr(ty);
                }
                self.push("),");
            }
            EnumVariant::Struct { name, fields, .. } => {
                self.push(&name.name);
                self.push(" {");
                self.newline();
                self.inc_indent();
                for field in fields {
                    self.format_record_field_decl(field);
                }
                self.dec_indent();
                self.push_indent();
                self.push("},");
            }
        }
        self.newline();
    }

    // ─── Classes ──────────────────────────────────────────────────────────

    fn format_class_decl(&mut self, decl: &ClassDecl) {
        self.format_annotations(&decl.annotations);
        self.push_indent();
        self.format_visibility(decl.visibility);
        self.push("class ");
        self.push(&decl.name.name);
        self.format_generic_params(&decl.generic_params);
        if let Some(base) = &decl.base {
            self.push(" extends ");
            self.format_type_path(base);
        }
        if !decl.traits.is_empty() {
            self.push(" impl ");
            for (i, t) in decl.traits.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.format_type_path(t);
            }
        }
        self.push(" {");
        self.newline();
        self.inc_indent();
        for field in &decl.fields {
            self.format_record_field_decl(field);
        }
        if !decl.fields.is_empty() && !decl.methods.is_empty() {
            self.newline();
        }
        for (i, method) in decl.methods.iter().enumerate() {
            if i > 0 {
                self.newline();
            }
            self.format_fn_decl(method);
        }
        self.dec_indent();
        self.push_line("}");
    }

    // ─── Traits ───────────────────────────────────────────────────────────

    fn format_trait_decl(&mut self, decl: &TraitDecl) {
        self.format_annotations(&decl.annotations);
        self.push_indent();
        self.format_visibility(decl.visibility);
        if decl.is_platform {
            self.push("platform ");
        }
        self.push("trait ");
        self.push(&decl.name.name);
        self.format_generic_params(&decl.generic_params);
        self.push(" {");
        self.newline();
        self.inc_indent();
        for assoc in &decl.associated_types {
            self.push_indent();
            self.push("type ");
            self.push(&assoc.name.name);
            if !assoc.bounds.is_empty() {
                self.push(": ");
                for (i, b) in assoc.bounds.iter().enumerate() {
                    if i > 0 {
                        self.push(" + ");
                    }
                    self.format_type_path(b);
                }
            }
            self.newline();
        }
        if !decl.associated_types.is_empty() && !decl.methods.is_empty() {
            self.newline();
        }
        for (i, method) in decl.methods.iter().enumerate() {
            if i > 0 {
                self.newline();
            }
            self.format_fn_decl(method);
        }
        self.dec_indent();
        self.push_line("}");
    }

    // ─── Impl blocks ─────────────────────────────────────────────────────

    fn format_impl_block(&mut self, decl: &ImplBlock) {
        self.format_annotations(&decl.annotations);
        self.push_indent();
        self.push("impl ");
        self.format_generic_params(&decl.generic_params);
        if !decl.generic_params.is_empty() {
            self.push_char(' ');
        }
        if let Some(trait_path) = &decl.trait_path {
            self.format_type_path(trait_path);
            self.push(" for ");
        }
        self.format_type_expr(&decl.target);
        if !decl.where_clause.is_empty() {
            self.newline();
            self.inc_indent();
            self.push_indent();
            self.push("where ");
            for (i, c) in decl.where_clause.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.format_type_constraint(c);
            }
            self.dec_indent();
        }
        self.push(" {");
        self.newline();
        self.inc_indent();
        for (i, method) in decl.methods.iter().enumerate() {
            if i > 0 {
                self.newline();
            }
            self.format_fn_decl(method);
        }
        self.dec_indent();
        self.push_line("}");
    }

    // ─── Effects ──────────────────────────────────────────────────────────

    fn format_effect_decl(&mut self, decl: &EffectDecl) {
        self.format_annotations(&decl.annotations);
        self.push_indent();
        self.format_visibility(decl.visibility);
        self.push("effect ");
        self.push(&decl.name.name);
        self.format_generic_params(&decl.generic_params);
        self.push(" {");
        self.newline();
        self.inc_indent();
        for (i, op) in decl.operations.iter().enumerate() {
            if i > 0 {
                self.newline();
            }
            self.format_fn_decl(op);
        }
        self.dec_indent();
        self.push_line("}");
    }

    // ─── Type aliases ─────────────────────────────────────────────────────

    fn format_type_alias(&mut self, decl: &TypeAliasDecl) {
        self.format_annotations(&decl.annotations);
        self.push_indent();
        self.format_visibility(decl.visibility);
        self.push("type ");
        self.push(&decl.name.name);
        self.format_generic_params(&decl.generic_params);
        self.push(" = ");
        self.format_type_expr(&decl.ty);
        if !decl.where_clause.is_empty() {
            self.push(" where ");
            for (i, c) in decl.where_clause.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.format_type_constraint(c);
            }
        }
        self.newline();
    }

    // ─── Const ────────────────────────────────────────────────────────────

    fn format_const_decl(&mut self, decl: &ConstDecl) {
        self.format_annotations(&decl.annotations);
        self.push_indent();
        self.format_visibility(decl.visibility);
        self.push("const ");
        self.push(&decl.name.name);
        self.push(": ");
        self.format_type_expr(&decl.ty);
        self.push(" = ");
        self.format_expr(&decl.value);
        self.newline();
    }

    // ─── Module handle ────────────────────────────────────────────────────

    fn format_module_handle(&mut self, decl: &ModuleHandleDecl) {
        self.push_indent();
        self.push("handle ");
        self.format_type_path(&decl.effect);
        self.push(" with ");
        self.format_expr(&decl.handler);
        self.newline();
    }

    // ─── Property test ────────────────────────────────────────────────────

    fn format_property_test(&mut self, decl: &PropertyTestDecl) {
        self.push_indent();
        self.push("property(\"");
        self.push(&decl.name);
        self.push("\") {");
        self.newline();
        self.inc_indent();
        if !decl.bindings.is_empty() {
            self.push_indent();
            self.push("forall(");
            for (i, b) in decl.bindings.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.push(&b.name.name);
                self.push(": ");
                self.format_type_expr(&b.ty);
            }
            self.push(") ");
            self.format_block(&decl.body);
            self.newline();
        } else {
            self.format_block_body(&decl.body);
        }
        self.dec_indent();
        self.push_line("}");
    }

    // ─── Type expressions ─────────────────────────────────────────────────

    fn format_type_expr(&mut self, ty: &TypeExpr) {
        match ty {
            TypeExpr::Named { path, args, .. } => {
                self.format_type_path(path);
                if !args.is_empty() {
                    self.push_char('[');
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            self.push(", ");
                        }
                        self.format_type_expr(arg);
                    }
                    self.push_char(']');
                }
            }
            TypeExpr::Tuple { elems, .. } => {
                self.push_char('(');
                for (i, elem) in elems.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.format_type_expr(elem);
                }
                self.push_char(')');
            }
            TypeExpr::Function {
                params,
                ret,
                effects,
                ..
            } => {
                self.push("Fn(");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.format_type_expr(p);
                }
                self.push(") -> ");
                self.format_type_expr(ret);
                if !effects.is_empty() {
                    self.push(" with ");
                    for (i, eff) in effects.iter().enumerate() {
                        if i > 0 {
                            self.push(", ");
                        }
                        self.format_type_path(eff);
                    }
                }
            }
            TypeExpr::Optional { inner, .. } => {
                self.format_type_expr(inner);
                self.push_char('?');
            }
            TypeExpr::SelfType { .. } => {
                self.push("Self");
            }
        }
    }

    fn format_type_path(&mut self, path: &TypePath) {
        for (i, seg) in path.segments.iter().enumerate() {
            if i > 0 {
                self.push_char('.');
            }
            self.push(&seg.name);
        }
    }

    // ─── Patterns ─────────────────────────────────────────────────────────

    fn format_pattern(&mut self, pat: &Pattern) {
        match pat {
            Pattern::Wildcard { .. } => self.push_char('_'),
            Pattern::Bind { name, .. } => self.push(&name.name),
            Pattern::MutBind { name, .. } => {
                self.push("mut ");
                self.push(&name.name);
            }
            Pattern::Literal { lit, .. } => self.format_literal(lit),
            Pattern::Constructor { path, fields, .. } => {
                self.format_type_path(path);
                self.push_char('(');
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.format_pattern(f);
                }
                self.push_char(')');
            }
            Pattern::Record {
                path, fields, rest, ..
            } => {
                self.format_type_path(path);
                self.push(" { ");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.push(&f.name.name);
                    if let Some(p) = &f.pattern {
                        self.push(": ");
                        self.format_pattern(p);
                    }
                }
                if *rest {
                    if !fields.is_empty() {
                        self.push(", ");
                    }
                    self.push("..");
                }
                self.push(" }");
            }
            Pattern::Tuple { elems, .. } => {
                self.push_char('(');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.format_pattern(e);
                }
                self.push_char(')');
            }
            Pattern::List { elems, rest, .. } => {
                self.push_char('[');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.format_pattern(e);
                }
                if let Some(r) = rest {
                    if !elems.is_empty() {
                        self.push(", ");
                    }
                    self.push("..");
                    self.format_pattern(r);
                }
                self.push_char(']');
            }
            Pattern::Or { alternatives, .. } => {
                for (i, alt) in alternatives.iter().enumerate() {
                    if i > 0 {
                        self.push(" | ");
                    }
                    self.format_pattern(alt);
                }
            }
            Pattern::Range {
                lo, hi, inclusive, ..
            } => {
                self.format_pattern(lo);
                if *inclusive {
                    self.push("..=");
                } else {
                    self.push("..");
                }
                self.format_pattern(hi);
            }
            Pattern::Rest { .. } => {
                self.push("..");
            }
        }
    }

    // ─── Expressions ──────────────────────────────────────────────────────

    fn format_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Literal { lit, .. } => self.format_literal(lit),
            Expr::Identifier { name, .. } => self.push(&name.name),
            Expr::Binary {
                op, left, right, ..
            } => {
                self.format_expr_maybe_paren(left, expr);
                self.push_char(' ');
                self.push(binop_str(*op));
                self.push_char(' ');
                self.format_expr_maybe_paren(right, expr);
            }
            Expr::Unary { op, operand, .. } => {
                self.push(unaryop_str(*op));
                self.format_expr(operand);
            }
            Expr::Assign {
                op, target, value, ..
            } => {
                self.format_expr(target);
                self.push_char(' ');
                self.push(assignop_str(*op));
                self.push_char(' ');
                self.format_expr(value);
            }
            Expr::Call {
                callee,
                args,
                type_args,
                ..
            } => {
                self.format_expr(callee);
                if !type_args.is_empty() {
                    self.push_char('[');
                    for (i, ta) in type_args.iter().enumerate() {
                        if i > 0 {
                            self.push(", ");
                        }
                        self.format_type_expr(ta);
                    }
                    self.push_char(']');
                }
                self.push_char('(');
                self.format_args(args);
                self.push_char(')');
            }
            Expr::MethodCall {
                receiver,
                method,
                type_args,
                args,
                ..
            } => {
                self.format_expr(receiver);
                self.push_char('.');
                self.push(&method.name);
                if !type_args.is_empty() {
                    self.push_char('[');
                    for (i, ta) in type_args.iter().enumerate() {
                        if i > 0 {
                            self.push(", ");
                        }
                        self.format_type_expr(ta);
                    }
                    self.push_char(']');
                }
                self.push_char('(');
                self.format_args(args);
                self.push_char(')');
            }
            Expr::FieldAccess { object, field, .. } => {
                self.format_expr(object);
                self.push_char('.');
                self.push(&field.name);
            }
            Expr::Index { object, index, .. } => {
                self.format_expr(object);
                self.push_char('[');
                self.format_expr(index);
                self.push_char(']');
            }
            Expr::Try { expr: inner, .. } => {
                self.format_expr(inner);
                self.push_char('?');
            }
            Expr::Lambda { params, body, .. } => {
                self.push_char('(');
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.format_param(p);
                }
                self.push(") => ");
                self.format_expr(body);
            }
            Expr::Pipe { left, right, .. } => {
                self.format_expr(left);
                self.newline();
                self.push_indent();
                self.push("|> ");
                self.format_expr(right);
            }
            Expr::Compose { left, right, .. } => {
                self.format_expr(left);
                self.push(" >> ");
                self.format_expr(right);
            }
            Expr::If {
                let_pattern,
                condition,
                then_block,
                else_block,
                ..
            } => {
                self.push("if ");
                if let Some(pat) = let_pattern {
                    self.push("let ");
                    self.format_pattern(pat);
                    self.push(" = ");
                }
                self.format_expr(condition);
                self.push(" ");
                self.format_block(then_block);
                if let Some(else_expr) = else_block {
                    self.push(" else ");
                    match else_expr.as_ref() {
                        Expr::If { .. } => {
                            self.format_expr(else_expr);
                        }
                        Expr::Block { block, .. } => {
                            self.format_block(block);
                        }
                        _ => {
                            self.format_expr(else_expr);
                        }
                    }
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.push("match ");
                self.format_expr(scrutinee);
                self.push(" {");
                self.newline();
                self.inc_indent();
                for arm in arms {
                    self.format_match_arm(arm);
                }
                self.dec_indent();
                self.push_indent();
                self.push_char('}');
            }
            Expr::Loop { body, .. } => {
                self.push("loop ");
                self.format_block(body);
            }
            Expr::Block { block, .. } => {
                self.format_block(block);
            }
            Expr::RecordConstruct {
                path,
                fields,
                spread,
                ..
            } => {
                self.format_type_path(path);
                self.push(" {");
                if fields.is_empty() && spread.is_none() {
                    self.push("}");
                } else {
                    self.newline();
                    self.inc_indent();
                    for field in fields {
                        self.push_indent();
                        self.push(&field.name.name);
                        if let Some(val) = &field.value {
                            self.push(": ");
                            self.format_expr(val);
                        }
                        self.push_char(',');
                        self.newline();
                    }
                    if let Some(spr) = spread {
                        self.push_indent();
                        self.push("..");
                        self.format_expr(&spr.expr);
                        self.push_char(',');
                        self.newline();
                    }
                    self.dec_indent();
                    self.push_indent();
                    self.push_char('}');
                }
            }
            Expr::ListLiteral { elems, .. } => {
                self.format_collection('[', ']', elems);
            }
            Expr::MapLiteral { entries, .. } => {
                if entries.is_empty() {
                    self.push("{}");
                } else {
                    self.push("{");
                    self.newline();
                    self.inc_indent();
                    for (k, v) in entries {
                        self.push_indent();
                        self.format_expr(k);
                        self.push(": ");
                        self.format_expr(v);
                        self.push_char(',');
                        self.newline();
                    }
                    self.dec_indent();
                    self.push_indent();
                    self.push("}");
                }
            }
            Expr::SetLiteral { elems, .. } => {
                if elems.is_empty() {
                    self.push("#{}");
                } else {
                    self.push("#{");
                    for (i, e) in elems.iter().enumerate() {
                        if i > 0 {
                            self.push(", ");
                        }
                        self.format_expr(e);
                    }
                    self.push("}");
                }
            }
            Expr::TupleLiteral { elems, .. } => {
                self.push_char('(');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.push(", ");
                    }
                    self.format_expr(e);
                }
                self.push_char(')');
            }
            Expr::Range {
                lo, hi, inclusive, ..
            } => {
                self.format_expr(lo);
                if *inclusive {
                    self.push("..=");
                } else {
                    self.push("..");
                }
                self.format_expr(hi);
            }
            Expr::Await { expr: inner, .. } => {
                self.push("await ");
                self.format_expr(inner);
            }
            Expr::Return { value, .. } => {
                self.push("return");
                if let Some(val) = value {
                    self.push_char(' ');
                    self.format_expr(val);
                }
            }
            Expr::Break { value, .. } => {
                self.push("break");
                if let Some(val) = value {
                    self.push_char(' ');
                    self.format_expr(val);
                }
            }
            Expr::Continue { .. } => {
                self.push("continue");
            }
            Expr::Unreachable { .. } => {
                self.push("unreachable");
            }
            Expr::Interpolation { parts, .. } => {
                self.push_char('"');
                for part in parts {
                    match part {
                        InterpolationPart::Literal(s) => self.push(s),
                        InterpolationPart::Expr(e) => {
                            self.push("${");
                            self.format_expr(e);
                            self.push_char('}');
                        }
                    }
                }
                self.push_char('"');
            }
            Expr::Placeholder { .. } => {
                self.push_char('_');
            }
            Expr::Is {
                expr, type_expr, ..
            } => {
                self.format_expr(expr);
                self.push(" is ");
                self.format_type_expr(type_expr);
            }
        }
    }

    /// Format an expression, adding parentheses if needed for precedence.
    fn format_expr_maybe_paren(&mut self, inner: &Expr, _parent: &Expr) {
        // For now, add parens around binary sub-expressions in binary contexts
        // to preserve correctness. A more sophisticated approach would compare
        // actual precedence levels.
        match inner {
            Expr::Binary { .. } | Expr::Assign { .. } => {
                // Check if inner is lower or different precedence than parent
                // For simplicity, we don't parenthesize same-level ops
                self.format_expr(inner);
            }
            _ => self.format_expr(inner),
        }
    }

    fn format_args(&mut self, args: &[Arg]) {
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            if arg.mutable {
                self.push("mut ");
            }
            if let Some(label) = &arg.label {
                self.push(&label.name);
                self.push(": ");
            }
            self.format_expr(&arg.value);
        }
    }

    fn format_collection(&mut self, open: char, close: char, elems: &[Expr]) {
        self.push_char(open);
        // Estimate total length to decide single vs multi-line
        let total_est: usize = elems.iter().map(|e| self.estimate_expr_len(e) + 2).sum();
        if total_est + self.indent * INDENT_WIDTH <= SOFT_LIMIT || elems.is_empty() {
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    self.push(", ");
                }
                self.format_expr(e);
            }
        } else {
            self.newline();
            self.inc_indent();
            for e in elems {
                self.push_indent();
                self.format_expr(e);
                self.push_char(',');
                self.newline();
            }
            self.dec_indent();
            self.push_indent();
        }
        self.push_char(close);
    }

    fn format_literal(&mut self, lit: &Literal) {
        match lit {
            Literal::Int(s) => self.push(s),
            Literal::Float(s) => self.push(s),
            Literal::Bool(b) => self.push(if *b { "true" } else { "false" }),
            Literal::Char(s) => {
                self.push_char('\'');
                self.push(s);
                self.push_char('\'');
            }
            Literal::String(s) => {
                self.push_char('"');
                self.push(&escape_string(s));
                self.push_char('"');
            }
            Literal::Unit => self.push("()"),
        }
    }

    fn format_match_arm(&mut self, arm: &MatchArm) {
        self.emit_comments_before(arm.span.start);
        self.push_indent();
        self.format_pattern(&arm.pattern);
        if let Some(guard) = &arm.guard {
            self.push(" if ");
            self.format_expr(guard);
        }
        self.push(" => ");
        match &arm.body {
            Expr::Block { block, .. } => {
                self.format_block(block);
                self.push_char(',');
            }
            _ => {
                self.format_expr(&arm.body);
                self.push_char(',');
            }
        }
        self.newline();
    }

    // ─── Blocks ───────────────────────────────────────────────────────────

    fn format_block(&mut self, block: &Block) {
        self.push_char('{');
        if block.stmts.is_empty() && block.tail.is_none() {
            self.push_char('}');
            return;
        }
        self.newline();
        self.inc_indent();
        self.format_block_body(block);
        self.dec_indent();
        self.push_indent();
        self.push_char('}');
    }

    fn format_block_body(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.format_stmt(stmt);
        }
        if let Some(tail) = &block.tail {
            self.emit_comments_before(tail.span().start);
            self.push_indent();
            self.format_expr(tail);
            self.newline();
        }
    }

    // ─── Statements ───────────────────────────────────────────────────────

    fn format_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let(let_stmt) => self.format_let_stmt(let_stmt),
            Stmt::Expr(expr) => {
                self.emit_comments_before(expr.span().start);
                self.push_indent();
                self.format_expr(expr);
                self.newline();
            }
            Stmt::For(for_loop) => self.format_for_loop(for_loop),
            Stmt::While(while_loop) => self.format_while_loop(while_loop),
            Stmt::Loop(loop_stmt) => self.format_loop_stmt(loop_stmt),
            Stmt::Guard(guard) => self.format_guard_stmt(guard),
            Stmt::Handling(handling) => self.format_handling_block(handling),
            Stmt::Empty => {}
        }
    }

    fn format_let_stmt(&mut self, stmt: &LetStmt) {
        self.emit_comments_before(stmt.span.start);
        self.push_indent();
        self.push("let ");
        self.format_pattern(&stmt.pattern);
        if let Some(ty) = &stmt.ty {
            self.push(": ");
            self.format_type_expr(ty);
        }
        self.push(" = ");
        self.format_expr(&stmt.value);
        self.newline();
    }

    fn format_for_loop(&mut self, stmt: &ForLoop) {
        self.emit_comments_before(stmt.span.start);
        self.push_indent();
        self.push("for ");
        self.format_pattern(&stmt.pattern);
        self.push(" in ");
        self.format_expr(&stmt.iterable);
        self.push(" ");
        self.format_block(&stmt.body);
        self.newline();
    }

    fn format_while_loop(&mut self, stmt: &WhileLoop) {
        self.emit_comments_before(stmt.span.start);
        self.push_indent();
        self.push("while ");
        self.format_expr(&stmt.condition);
        self.push(" ");
        self.format_block(&stmt.body);
        self.newline();
    }

    fn format_loop_stmt(&mut self, stmt: &LoopStmt) {
        self.emit_comments_before(stmt.span.start);
        self.push_indent();
        self.push("loop ");
        self.format_block(&stmt.body);
        self.newline();
    }

    fn format_guard_stmt(&mut self, stmt: &GuardStmt) {
        self.emit_comments_before(stmt.span.start);
        self.push_indent();
        if let Some(pat) = &stmt.let_pattern {
            self.push("guard (let ");
            self.format_pattern(pat);
            self.push(" = ");
            self.format_expr(&stmt.condition);
            self.push(") else ");
        } else {
            self.push("guard ");
            self.format_expr(&stmt.condition);
            self.push(" else ");
        }
        self.format_block(&stmt.else_block);
        self.newline();
    }

    fn format_handling_block(&mut self, stmt: &HandlingBlock) {
        self.emit_comments_before(stmt.span.start);
        self.push_indent();
        self.push("handling (");
        for (i, h) in stmt.handlers.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.format_type_path(&h.effect);
            self.push(" with ");
            self.format_expr(&h.handler);
        }
        self.push(") ");
        self.format_block(&stmt.body);
        self.newline();
    }
}

// ─── Helper functions ─────────────────────────────────────────────────────

fn binop_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Pow => "**",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Compose => ">>",
        BinOp::Is => "is",
    }
}

fn unaryop_str(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "!",
        UnaryOp::BitNot => "~",
    }
}

fn assignop_str(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Assign => "=",
        AssignOp::AddAssign => "+=",
        AssignOp::SubAssign => "-=",
        AssignOp::MulAssign => "*=",
        AssignOp::DivAssign => "/=",
        AssignOp::RemAssign => "%=",
    }
}

/// Import category for sorting: 0=core, 1=std, 2=external, 3=local.
fn import_category(import: &ImportDecl) -> u8 {
    let first = import
        .path
        .segments
        .first()
        .map(|s| s.name.as_str())
        .unwrap_or("");
    match first {
        "Core" => 0,
        "Std" => 1,
        _ => {
            // Uppercase initial = external package, lowercase = local module
            if first.starts_with(|c: char| c.is_uppercase()) {
                2
            } else {
                3
            }
        }
    }
}

fn import_path_str(import: &ImportDecl) -> String {
    import
        .path
        .segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

/// Wrap lines that exceed the hard limit (100 chars).
///
/// For each line longer than `HARD_LIMIT`, finds the best natural break point
/// (after commas, before operators, before `.`) and wraps there. Continuation
/// lines are indented by the original line's indentation plus 4 extra spaces.
fn wrap_long_lines(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for line in input.lines() {
        if line.len() <= HARD_LIMIT {
            output.push_str(line);
            output.push('\n');
        } else {
            wrap_single_line(line, &mut output);
        }
    }
    output
}

/// Wrap a single line that exceeds the hard limit.
fn wrap_single_line(line: &str, output: &mut String) {
    let base_indent = line.len() - line.trim_start().len();
    let continuation_indent = base_indent + INDENT_WIDTH * 2;
    let cont_prefix: String = " ".repeat(continuation_indent);

    let mut remaining = line.to_string();
    while remaining.len() > HARD_LIMIT {
        if let Some(pos) = find_break_point(&remaining, HARD_LIMIT) {
            let (before, after) = split_at_break(&remaining, pos);
            output.push_str(&before);
            output.push('\n');
            remaining = format!("{cont_prefix}{after}");
        } else {
            // No natural break point found — emit as-is to avoid infinite loop
            break;
        }
    }
    output.push_str(&remaining);
    output.push('\n');
}

/// Find the best break position at or before `limit` in `line`.
///
/// Prefers breaking at natural points:
/// - After `, ` (position after the space)
/// - Before binary operators like ` + `, ` - `, ` && `, ` || `, etc.
/// - Before `.` in method chains
///
/// Returns the byte offset where the break should occur (content before this
/// offset goes on the current line, content from this offset goes on the next).
fn find_break_point(line: &str, limit: usize) -> Option<usize> {
    if line.len() <= limit {
        return None;
    }

    // Search window: look backwards from limit to find natural break points.
    // We look within the line up to `limit` chars.
    let search_end = limit.min(line.len());

    // Track the best candidate break position
    let mut best: Option<usize> = None;

    // Scan for break candidates within the limit.
    // We prefer breaks closer to the limit (rightmost).
    let bytes = line.as_bytes();

    // 1. After ", " — break after the space (next content starts new line)
    for i in 0..search_end.saturating_sub(1) {
        if bytes[i] == b',' && i + 1 < search_end && bytes[i + 1] == b' ' {
            let candidate = i + 2; // break after ", "
            if candidate <= limit {
                best = Some(candidate);
            }
        }
    }

    // 2. Before binary operators: " + ", " - ", " * ", " / ", " % ",
    //    " && ", " || ", " == ", " != ", " <= ", " >= ", " < ", " > ",
    //    " = ", " => ", " |> ", " >> "
    let op_patterns: &[&str] = &[
        " && ", " || ", " == ", " != ", " <= ", " >= ", " |> ", " >> ", " => ", " + ", " - ",
        " * ", " / ", " % ", " < ", " > ", " = ",
    ];
    for pat in op_patterns {
        // Find all occurrences within the search window
        let mut start = 0;
        while let Some(pos) = line[start..search_end].find(pat) {
            let abs_pos = start + pos;
            // Break before the operator (at the space before it)
            let candidate = abs_pos + 1; // after the leading space, break before op
            if candidate <= limit
                && candidate > 0
                && (best.is_none() || candidate > best.unwrap_or(0))
            {
                best = Some(candidate);
            }
            start = abs_pos + 1;
        }
    }

    // 3. Before "." (method chain) — but not ".." (range)
    for i in 1..search_end {
        if bytes[i] == b'.' && (i + 1 >= line.len() || bytes[i + 1] != b'.') && bytes[i - 1] != b'.'
        {
            let candidate = i; // break before "."
            if candidate <= limit
                && candidate > 0
                && (best.is_none() || candidate > best.unwrap_or(0))
            {
                best = Some(candidate);
            }
        }
    }

    // Only accept break if it's past the indentation
    let content_start = line.len() - line.trim_start().len();
    best.filter(|&pos| pos > content_start)
}

/// Split a line at a break position, trimming trailing whitespace from the
/// first part and leading whitespace from the second.
fn split_at_break(line: &str, pos: usize) -> (String, String) {
    let before = line[..pos].trim_end().to_string();
    let after = line[pos..].trim_start().to_string();
    (before, after)
}

/// Escape special characters in a string literal.
fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}
