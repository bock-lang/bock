//! Implementation of the `bock repl` command.
//!
//! Provides an interactive REPL with persistent environment, line editing
//! via `rustyline`, and special commands (`:type`, `:air`, `:paste`, etc.).

use std::path::PathBuf;

use bock_air::{
    lower_module, resolve_names, Binding, NameKind, NodeIdGen, NodeKind, ResolvedName, SymbolTable,
};
use bock_ast::Visibility;
use bock_errors::{Diagnostic, DiagnosticBag, FileId, Severity, Span};
use bock_interp::{Interpreter, Value};
use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceMap;
use bock_types::{FnType, PrimitiveType, Type, TypeChecker};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

/// Run the interactive REPL.
pub async fn run() -> anyhow::Result<()> {
    let mut repl = Repl::new()?;
    repl.run().await
}

/// Persistent REPL state across input lines.
struct Repl {
    editor: DefaultEditor,
    interp: Interpreter,
    symbols: SymbolTable,
    checker: TypeChecker,
    id_gen: NodeIdGen,
    line_counter: u32,
    history_path: Option<PathBuf>,
}

impl Repl {
    fn new() -> anyhow::Result<Self> {
        let mut editor = DefaultEditor::new()?;

        // Try to load history from ~/.bock_history
        let history_path = dirs_or_home().map(|p| p.join(".bock_history"));
        if let Some(ref path) = history_path {
            let _ = editor.load_history(path);
        }

        let mut symbols = SymbolTable::new();
        register_builtins(&mut symbols);

        let mut checker = TypeChecker::new();
        register_type_builtins(&mut checker);

        let mut interp = Interpreter::new();
        bock_core::register_core(&mut interp.builtins);
        Ok(Self {
            editor,
            interp,
            symbols,
            checker,
            id_gen: NodeIdGen::new(),
            line_counter: 0,
            history_path,
        })
    }

    async fn run(&mut self) -> anyhow::Result<()> {
        println!("Bock REPL v0.1.0");
        println!("Type :help for available commands, :quit to exit.\n");

        loop {
            let prompt = "bock> ";
            let readline = self.editor.readline(prompt);

            match readline {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    let _ = self.editor.add_history_entry(&line);

                    if trimmed.starts_with(':') {
                        if self.handle_command(trimmed).await {
                            break;
                        }
                    } else {
                        self.eval_line(trimmed).await;
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    println!("^C");
                }
                Err(ReadlineError::Eof) => {
                    println!("Bye!");
                    break;
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    break;
                }
            }
        }

        // Save history
        if let Some(ref path) = self.history_path {
            let _ = self.editor.save_history(path);
        }

        Ok(())
    }

    /// Handle a REPL command. Returns `true` if the REPL should exit.
    async fn handle_command(&mut self, input: &str) -> bool {
        let (cmd, arg) = split_command(input);

        match cmd {
            ":quit" | ":q" => return true,

            ":help" | ":h" => {
                println!("REPL commands:");
                println!("  :type <expr>     Show the inferred type of an expression");
                println!("  :air <stmt>      Show the AIR representation");
                println!("  :target <T> <s>  Show target-specific output (stub)");
                println!("  :effects         Show registered effects");
                println!("  :context         Show current variable bindings");
                println!("  :load <file>     Load and execute a Bock file");
                println!("  :paste           Enter multi-line paste mode");
                println!("  :quit            Exit the REPL");
            }

            ":type" | ":t" => {
                if let Some(expr) = arg {
                    self.show_type(expr);
                } else {
                    println!("Usage: :type <expression>");
                }
            }

            ":air" => {
                if let Some(stmt) = arg {
                    self.show_air(stmt);
                } else {
                    println!("Usage: :air <statement>");
                }
            }

            ":target" => {
                println!("Target-specific output is not yet implemented.");
            }

            ":effects" => {
                println!("Registered effects:");
                let handlers = &self.interp.effect_handlers;
                if handlers.is_empty() {
                    println!("  (none)");
                } else {
                    println!("  {handlers:?}");
                }
            }

            ":context" | ":ctx" => {
                self.show_context();
            }

            ":load" | ":l" => {
                if let Some(path) = arg {
                    self.load_file(path).await;
                } else {
                    println!("Usage: :load <filename>");
                }
            }

            ":paste" | ":p" => {
                self.paste_mode().await;
            }

            _ => {
                println!("Unknown command: {cmd}. Type :help for available commands.");
            }
        }

        false
    }

    /// Evaluate a line of Bock code in the persistent environment.
    async fn eval_line(&mut self, input: &str) {
        self.line_counter += 1;
        let filename = format!("<repl:{}>", self.line_counter);

        // Try parsing as a top-level item first (fn, record, enum, etc.)
        // If that fails, wrap in a function body to parse as expression/statement.
        let is_item = is_likely_item(input);

        let source = if is_item {
            input.to_string()
        } else {
            // Wrap expression/statement in a function so the parser can handle it.
            // Use a unique name to avoid collisions.
            format!("fn __repl_{n}__() {{ {input} }}", n = self.line_counter)
        };

        match self.run_pipeline(&source, &filename, is_item).await {
            Ok(result) => {
                if let Some(val) = result {
                    if val != Value::Void {
                        println!("{val}");
                    }
                }
            }
            Err(msg) => {
                eprintln!("{msg}");
            }
        }
    }

    /// Run the full compilation pipeline on a source string.
    /// Returns `Ok(Some(value))` for expressions, `Ok(None)` for declarations.
    async fn run_pipeline(
        &mut self,
        source: &str,
        filename: &str,
        is_item: bool,
    ) -> Result<Option<Value>, String> {
        let mut source_map = SourceMap::new();
        let file_id = source_map.add_file(PathBuf::from(filename), source.to_string());
        let source_file = source_map.get_file(file_id);

        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Lex
        let mut lexer = Lexer::new(source_file);
        let tokens = lexer.tokenize();
        collect_diagnostics(&mut diagnostics, lexer.diagnostics());
        if has_errors(&diagnostics) {
            return Err(format_diagnostics(
                &diagnostics,
                filename,
                &source_file.content,
            ));
        }

        // Parse
        let mut parser = Parser::new(tokens, source_file);
        let module = parser.parse_module();
        collect_diagnostics(&mut diagnostics, parser.diagnostics());
        if has_errors(&diagnostics) {
            return Err(format_diagnostics(
                &diagnostics,
                filename,
                &source_file.content,
            ));
        }

        // Name resolution
        let resolve_diags = resolve_names(&module, &mut self.symbols);
        collect_diagnostics(&mut diagnostics, &resolve_diags);
        if has_errors(&diagnostics) {
            return Err(format_diagnostics(
                &diagnostics,
                filename,
                &source_file.content,
            ));
        }

        // Lower to S-AIR
        let mut air_module = lower_module(&module, &self.id_gen, &self.symbols);

        // Type check — only for top-level items (function declarations, etc.).
        // REPL wrapper functions have no return-type annotation, which causes
        // false type-mismatch errors for bare expressions.
        if is_item {
            self.checker.check_module(&mut air_module);
            collect_diagnostics(&mut diagnostics, &self.checker.diags);
            self.checker.diags = DiagnosticBag::new();
            if has_errors(&diagnostics) {
                return Err(format_diagnostics(
                    &diagnostics,
                    filename,
                    &source_file.content,
                ));
            }

            // Print warnings
            let warnings: Vec<&Diagnostic> = diagnostics
                .iter()
                .filter(|d| d.severity != Severity::Error)
                .collect();
            if !warnings.is_empty() {
                let to_render: Vec<Diagnostic> = warnings.into_iter().cloned().collect();
                eprint!(
                    "{}",
                    format_diagnostics(&to_render, filename, &source_file.content)
                );
            }
        }

        // Execute
        let items = match &air_module.kind {
            NodeKind::Module { items, .. } => items.clone(),
            _ => return Err("internal error: expected Module node".to_string()),
        };

        if is_item {
            // Top-level items: register functions, execute let bindings, etc.
            for item in &items {
                match &item.kind {
                    NodeKind::FnDecl {
                        name,
                        params,
                        body,
                        is_async,
                        ..
                    } => {
                        let param_names: Vec<String> =
                            params.iter().filter_map(extract_param_name).collect();
                        self.interp.register_fn_with_async(
                            &name.name,
                            param_names,
                            *body.clone(),
                            *is_async,
                        );
                    }
                    NodeKind::EnumDecl { name, variants, .. } => {
                        self.interp.register_enum(&name.name, variants);
                    }
                    NodeKind::ConstDecl { name, value, .. } => {
                        match self.interp.eval_expr(value).await {
                            Ok(val) => self.interp.env.define(&name.name, val),
                            Err(e) => return Err(format!("runtime error: {e}")),
                        }
                    }
                    NodeKind::ImplBlock {
                        target, methods, ..
                    } => {
                        self.interp.register_impl(target, methods);
                    }
                    NodeKind::LetBinding { .. } | NodeKind::ModuleHandle { .. } => {
                        if let Err(e) = self.interp.eval_expr(item).await {
                            return Err(format!("runtime error: {e}"));
                        }
                    }
                    _ => {}
                }
            }
            Ok(None)
        } else {
            // Wrapped expression/statement: extract from the __repl_N__ function body.
            // The module should contain one fn with our wrapped code.
            for item in &items {
                if let NodeKind::FnDecl { body, .. } = &item.kind {
                    return self.eval_body(body).await;
                }
            }
            Ok(None)
        }
    }

    /// Evaluate the body of the REPL wrapper function.
    async fn eval_body(&mut self, body: &bock_air::AIRNode) -> Result<Option<Value>, String> {
        // The body is a Block node. Execute its statements and return the result.
        match &body.kind {
            NodeKind::Block { stmts, tail } => {
                // Execute all statements
                for stmt in stmts {
                    match self.interp.eval_expr(stmt).await {
                        Ok(_) => {}
                        Err(bock_interp::RuntimeError::Return(val)) => {
                            return Ok(Some(*val));
                        }
                        Err(e) => return Err(format!("runtime error: {e}")),
                    }
                }
                // Evaluate tail expression if present
                if let Some(tail_expr) = tail {
                    match self.interp.eval_expr(tail_expr).await {
                        Ok(val) => Ok(Some(val)),
                        Err(bock_interp::RuntimeError::Return(val)) => Ok(Some(*val)),
                        Err(e) => Err(format!("runtime error: {e}")),
                    }
                } else {
                    Ok(None)
                }
            }
            _ => {
                // Single expression
                match self.interp.eval_expr(body).await {
                    Ok(val) => Ok(Some(val)),
                    Err(e) => Err(format!("runtime error: {e}")),
                }
            }
        }
    }

    /// Show the inferred type of an expression.
    fn show_type(&mut self, input: &str) {
        self.line_counter += 1;
        let filename = format!("<repl:type:{}>", self.line_counter);
        let source = format!(
            "fn __repl_type_{n}__() {{ {input} }}",
            n = self.line_counter
        );

        let mut source_map = SourceMap::new();
        let file_id = source_map.add_file(PathBuf::from(&filename), source.clone());
        let source_file = source_map.get_file(file_id);

        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Lex
        let mut lexer = Lexer::new(source_file);
        let tokens = lexer.tokenize();
        collect_diagnostics(&mut diagnostics, lexer.diagnostics());
        if has_errors(&diagnostics) {
            eprintln!(
                "{}",
                format_diagnostics(&diagnostics, &filename, &source_file.content)
            );
            return;
        }

        // Parse
        let mut parser = Parser::new(tokens, source_file);
        let module = parser.parse_module();
        collect_diagnostics(&mut diagnostics, parser.diagnostics());
        if has_errors(&diagnostics) {
            eprintln!(
                "{}",
                format_diagnostics(&diagnostics, &filename, &source_file.content)
            );
            return;
        }

        // Name resolution
        let resolve_diags = resolve_names(&module, &mut self.symbols);
        collect_diagnostics(&mut diagnostics, &resolve_diags);
        if has_errors(&diagnostics) {
            eprintln!(
                "{}",
                format_diagnostics(&diagnostics, &filename, &source_file.content)
            );
            return;
        }

        // Lower
        let mut air_module = lower_module(&module, &self.id_gen, &self.symbols);

        // Type check
        self.checker.check_module(&mut air_module);
        collect_diagnostics(&mut diagnostics, &self.checker.diags);
        self.checker.diags = DiagnosticBag::new();

        if has_errors(&diagnostics) {
            eprintln!(
                "{}",
                format_diagnostics(&diagnostics, &filename, &source_file.content)
            );
            return;
        }

        // Find the expression in the wrapper function body and get its type
        let items = match &air_module.kind {
            NodeKind::Module { items, .. } => items.clone(),
            _ => return,
        };

        for item in &items {
            if let NodeKind::FnDecl { body, .. } = &item.kind {
                // Get the tail expression or last statement from the body
                if let Some(node_id) = get_tail_node_id(body) {
                    if let Some(ty) = self.checker.type_of(node_id) {
                        let resolved = self.checker.subst.apply(ty);
                        println!("{}", format_type(&resolved));
                        return;
                    }
                }
                // Fallback: try to infer from the body itself
                if let Some(ty) = self.checker.type_of(body.id) {
                    let resolved = self.checker.subst.apply(ty);
                    println!("{}", format_type(&resolved));
                } else {
                    println!("(unable to determine type)");
                }
                return;
            }
        }
    }

    /// Show the AIR representation of a statement.
    fn show_air(&mut self, input: &str) {
        self.line_counter += 1;
        let filename = format!("<repl:air:{}>", self.line_counter);

        let is_item = is_likely_item(input);
        let source = if is_item {
            input.to_string()
        } else {
            format!("fn __repl_air_{n}__() {{ {input} }}", n = self.line_counter)
        };

        let mut source_map = SourceMap::new();
        let file_id = source_map.add_file(PathBuf::from(&filename), source.clone());
        let source_file = source_map.get_file(file_id);

        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Lex
        let mut lexer = Lexer::new(source_file);
        let tokens = lexer.tokenize();
        collect_diagnostics(&mut diagnostics, lexer.diagnostics());
        if has_errors(&diagnostics) {
            eprintln!(
                "{}",
                format_diagnostics(&diagnostics, &filename, &source_file.content)
            );
            return;
        }

        // Parse
        let mut parser = Parser::new(tokens, source_file);
        let module = parser.parse_module();
        collect_diagnostics(&mut diagnostics, parser.diagnostics());
        if has_errors(&diagnostics) {
            eprintln!(
                "{}",
                format_diagnostics(&diagnostics, &filename, &source_file.content)
            );
            return;
        }

        // Name resolution
        let resolve_diags = resolve_names(&module, &mut self.symbols);
        collect_diagnostics(&mut diagnostics, &resolve_diags);
        if has_errors(&diagnostics) {
            eprintln!(
                "{}",
                format_diagnostics(&diagnostics, &filename, &source_file.content)
            );
            return;
        }

        // Lower
        let air_module = lower_module(&module, &self.id_gen, &self.symbols);

        // Print the AIR
        let items = match &air_module.kind {
            NodeKind::Module { items, .. } => items,
            _ => return,
        };

        if is_item {
            for item in items {
                println!("{item:#?}");
            }
        } else {
            // Extract the body from the wrapper function
            for item in items {
                if let NodeKind::FnDecl { body, .. } = &item.kind {
                    println!("{body:#?}");
                    return;
                }
            }
        }
    }

    /// Display current variable bindings.
    fn show_context(&self) {
        println!("Current bindings:");
        let bindings = self.interp.env.all_bindings();
        if bindings.is_empty() {
            println!("  (empty)");
        } else {
            for (name, value) in &bindings {
                // Skip internal REPL functions and builtins
                if name.starts_with("__repl_") {
                    continue;
                }
                println!("  {name} = {value}");
            }
        }
    }

    /// Load and execute a Bock source file.
    async fn load_file(&mut self, path: &str) {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: cannot read {path}: {e}");
                return;
            }
        };

        match self.run_pipeline(&content, path, true).await {
            Ok(_) => println!("Loaded {path}"),
            Err(msg) => eprintln!("{msg}"),
        }
    }

    /// Enter multi-line paste mode.
    async fn paste_mode(&mut self) {
        println!("-- Entering paste mode. Type a blank line to evaluate. --");
        let mut lines = Vec::new();

        loop {
            match self.editor.readline("| ") {
                Ok(line) => {
                    if line.trim().is_empty() {
                        break;
                    }
                    lines.push(line);
                }
                Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                    println!("-- Paste mode cancelled. --");
                    return;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return;
                }
            }
        }

        if lines.is_empty() {
            return;
        }

        let input = lines.join("\n");
        let _ = self.editor.add_history_entry(&input);
        println!("-- Evaluating... --");
        self.eval_line(&input).await;
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Heuristic: does this line look like a top-level item (fn, record, enum, etc.)?
fn is_likely_item(input: &str) -> bool {
    let trimmed = input.trim_start();
    let keywords = [
        "fn ",
        "pub fn ",
        "record ",
        "pub record ",
        "enum ",
        "pub enum ",
        "class ",
        "pub class ",
        "trait ",
        "pub trait ",
        "impl ",
        "effect ",
        "pub effect ",
        "type ",
        "pub type ",
        "const ",
        "pub const ",
        "import ",
        "from ",
        "handle ",
        "property(",
    ];
    keywords.iter().any(|kw| trimmed.starts_with(kw))
}

/// Split a REPL command into the command name and optional argument.
fn split_command(input: &str) -> (&str, Option<&str>) {
    if let Some(idx) = input.find(char::is_whitespace) {
        let cmd = &input[..idx];
        let arg = input[idx..].trim();
        if arg.is_empty() {
            (cmd, None)
        } else {
            (cmd, Some(arg))
        }
    } else {
        (input, None)
    }
}

/// Get the home directory for history storage.
fn dirs_or_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Extract the node ID of the tail expression (or last statement) in a block.
fn get_tail_node_id(body: &bock_air::AIRNode) -> Option<bock_air::NodeId> {
    match &body.kind {
        NodeKind::Block { stmts, tail } => {
            if let Some(tail_expr) = tail {
                Some(tail_expr.id)
            } else {
                stmts.last().map(|s| s.id)
            }
        }
        _ => Some(body.id),
    }
}

/// Extract the parameter name from a Param AIR node.
fn extract_param_name(param: &bock_air::AIRNode) -> Option<String> {
    if let NodeKind::Param { pattern, .. } = &param.kind {
        if let NodeKind::BindPat { name, .. } = &pattern.kind {
            return Some(name.name.clone());
        }
    }
    None
}

/// Format a Type for display.
fn format_type(ty: &Type) -> String {
    match ty {
        Type::Primitive(p) => format!("{p:?}"),
        Type::Named(n) => n.name.clone(),
        Type::Generic(g) => {
            let args: Vec<String> = g.args.iter().map(format_type).collect();
            format!("{}[{}]", g.constructor, args.join(", "))
        }
        Type::Tuple(tys) => {
            let inner: Vec<String> = tys.iter().map(format_type).collect();
            format!("({})", inner.join(", "))
        }
        Type::Function(f) => {
            let params: Vec<String> = f.params.iter().map(format_type).collect();
            let ret = format_type(&f.ret);
            format!("Fn({}) -> {ret}", params.join(", "))
        }
        Type::Optional(inner) => format!("{}?", format_type(inner)),
        Type::Result(ok, err) => format!("Result[{}, {}]", format_type(ok), format_type(err)),
        Type::TypeVar(id) => format!("?{id}"),
        Type::Refined(base, pred) => format!("{} where {}", format_type(base), pred.source),
        Type::Flexible(_) => "flexible".to_string(),
        Type::Error => "<error>".to_string(),
    }
}

/// Collect diagnostics from a bag into the accumulator.
fn collect_diagnostics(acc: &mut Vec<Diagnostic>, bag: &DiagnosticBag) {
    for diag in bag.iter() {
        acc.push(diag.clone());
    }
}

/// Check if any diagnostic is an error.
fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == Severity::Error)
}

/// Format diagnostics into a human-readable string.
fn format_diagnostics(diagnostics: &[Diagnostic], filename: &str, source: &str) -> String {
    bock_errors::render(diagnostics, filename, source)
}

/// Register builtin functions in the symbol table.
fn register_builtins(symbols: &mut SymbolTable) {
    let builtin_span = Span {
        file: FileId(0),
        start: 0,
        end: 0,
    };
    let builtins = [
        ("print", u32::MAX - 1),
        ("println", u32::MAX - 2),
        ("debug", u32::MAX - 3),
    ];
    for (name, def_id) in builtins {
        symbols.define(
            name.to_string(),
            Binding {
                name: name.to_string(),
                resolved: ResolvedName {
                    def_id,
                    kind: NameKind::Function,
                },
                visibility: Visibility::Public,
                span: builtin_span,
                used: true,
                is_import: false,
            },
        );
    }
}

/// Register builtin function types in the type checker.
fn register_type_builtins(checker: &mut TypeChecker) {
    let io_fn_ty = Type::Function(FnType {
        params: vec![Type::Primitive(PrimitiveType::String)],
        ret: Box::new(Type::Primitive(PrimitiveType::Void)),
        effects: vec![],
    });
    for name in ["print", "println", "debug"] {
        checker.env.define(name, io_fn_ty.clone());
    }

    // Duration, Instant, Channel: named prelude types. Type::Error accepts
    // any method or associated-function access without friction; runtime
    // dispatch via qualified globals handles correctness.
    for name in ["Duration", "Instant", "Channel"] {
        checker.env.define(name, Type::Error);
    }

    // sleep: (Duration) -> Void with Clock (effect elided at this layer)
    let sleep_fn_ty = Type::Function(FnType {
        params: vec![Type::Error],
        ret: Box::new(Type::Primitive(PrimitiveType::Void)),
        effects: vec![],
    });
    checker.env.define("sleep", sleep_fn_ty);

    // spawn: (Future[T]) -> Future[T]
    let spawn_fn_ty = Type::Function(FnType {
        params: vec![Type::Error],
        ret: Box::new(Type::Error),
        effects: vec![],
    });
    checker.env.define("spawn", spawn_fn_ty);

    // assert: (Bool, String?) -> Void
    let assert_ty = Type::Function(FnType {
        params: vec![Type::Primitive(PrimitiveType::Bool)],
        ret: Box::new(Type::Primitive(PrimitiveType::Void)),
        effects: vec![],
    });
    checker.env.define("assert", assert_ty);

    // expect: (Any) -> Expectation (test assertion builtin)
    let expect_fn_ty = Type::Function(FnType {
        params: vec![Type::Error],
        ret: Box::new(Type::Error),
        effects: vec![],
    });
    checker.env.define("expect", expect_fn_ty);

    // todo, unreachable: () -> Never (diverging builtins)
    let never_fn_ty = Type::Function(FnType {
        params: vec![],
        ret: Box::new(Type::Primitive(PrimitiveType::Never)),
        effects: vec![],
    });
    for name in ["todo", "unreachable"] {
        checker.env.define(name, never_fn_ty.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_is_likely_item() {
        assert!(is_likely_item("fn foo() { 1 }"));
        assert!(is_likely_item("pub fn bar() { }"));
        assert!(is_likely_item("record Point { x: Int, y: Int }"));
        assert!(is_likely_item("enum Color { Red, Green, Blue }"));
        assert!(!is_likely_item("1 + 2"));
        assert!(!is_likely_item("let x = 5"));
        assert!(!is_likely_item("x * y"));
    }

    #[tokio::test]
    async fn test_split_command() {
        assert_eq!(split_command(":quit"), (":quit", None));
        assert_eq!(split_command(":type 1 + 2"), (":type", Some("1 + 2")));
        assert_eq!(split_command(":load foo.bock"), (":load", Some("foo.bock")));
        assert_eq!(split_command(":help"), (":help", None));
    }

    #[tokio::test]
    async fn test_format_type() {
        assert_eq!(format_type(&Type::Primitive(PrimitiveType::Int)), "Int");
        assert_eq!(
            format_type(&Type::Primitive(PrimitiveType::String)),
            "String"
        );
        assert_eq!(format_type(&Type::Primitive(PrimitiveType::Bool)), "Bool");
        assert_eq!(
            format_type(&Type::Optional(Box::new(Type::Primitive(
                PrimitiveType::Int
            )))),
            "Int?"
        );
    }

    #[tokio::test]
    async fn test_pipeline_expression() {
        let mut repl_state = Repl::new().unwrap();
        let result = repl_state
            .run_pipeline("fn __repl_1__() { 42 }", "<test>", false)
            .await;
        assert!(result.is_ok(), "pipeline error: {}", result.unwrap_err());
        let val = result.unwrap();
        assert_eq!(val, Some(Value::Int(42)));
    }

    #[tokio::test]
    async fn test_pipeline_let_binding() {
        let mut repl_state = Repl::new().unwrap();
        // First define a variable
        let result = repl_state
            .run_pipeline("fn __repl_1__() { let x = 10 }", "<test>", false)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_pipeline_fn_declaration() {
        let mut repl_state = Repl::new().unwrap();
        let result = repl_state
            .run_pipeline("fn double(x: Int) -> Int { x * 2 }", "<test>", true)
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
        // Verify function is registered
        assert!(repl_state.interp.env.get("double").is_some());
    }

    #[tokio::test]
    async fn test_pipeline_parse_error() {
        let mut repl_state = Repl::new().unwrap();
        let result = repl_state
            .run_pipeline("fn __repl_1__() { @@@invalid }", "<test>", false)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_persistent_environment() {
        let mut repl_state = Repl::new().unwrap();

        // Define a function
        let _ = repl_state
            .run_pipeline("fn add(a: Int, b: Int) -> Int { a + b }", "<test>", true)
            .await;

        // The function should be callable
        assert!(repl_state.interp.env.get("add").is_some());
    }
}
