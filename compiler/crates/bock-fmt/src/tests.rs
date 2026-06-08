//! Tests for the Bock formatter.
//!
//! Each test provides a "before" (possibly messy) input and verifies the formatter
//! produces the expected "after" output. Idempotency is also checked.

use crate::format_source;

/// Helper: format source and check expected output + idempotency.
fn check(input: &str, expected: &str) {
    let result = format_source(input, "test.bock");
    assert_eq!(
        result.output, expected,
        "\n--- EXPECTED ---\n{expected}\n--- GOT ---\n{}\n",
        result.output
    );
    // Idempotency: formatting the output again should produce the same result
    let result2 = format_source(&result.output, "test.bock");
    assert_eq!(
        result2.output, result.output,
        "Formatter is not idempotent!\n--- FIRST ---\n{}\n--- SECOND ---\n{}\n",
        result.output, result2.output
    );
    assert!(
        !result2.changed,
        "Idempotent format should report no changes"
    );
}

// ─── Functions ────────────────────────────────────────────────────────────

#[test]
fn format_simple_fn() {
    check(
        "fn greet(name: String) -> String {\n  \"hello\"\n}\n",
        "fn greet(name: String) -> String {\n  \"hello\"\n}\n",
    );
}

#[test]
fn format_fn_public_async() {
    // Bock's visibility keyword is `public` (not Rust's `pub`); the formatter
    // must preserve it verbatim so the output re-parses.
    check(
        "public async fn fetch(url: String) -> String {\n  url\n}\n",
        "public async fn fetch(url: String) -> String {\n  url\n}\n",
    );
}

#[test]
fn format_fn_no_params() {
    check("fn hello() {\n  42\n}\n", "fn hello() {\n  42\n}\n");
}

// ─── Let bindings ─────────────────────────────────────────────────────────

#[test]
fn format_let_binding() {
    check(
        "fn main() {\n  let   x   =   42\n}\n",
        "fn main() {\n  let x = 42\n}\n",
    );
}

#[test]
fn format_let_with_type() {
    check(
        "fn main() {\n  let x: Int = 42\n}\n",
        "fn main() {\n  let x: Int = 42\n}\n",
    );
}

// ─── Records ──────────────────────────────────────────────────────────────

#[test]
fn format_record() {
    check(
        "record Point {\n  x: Float,\n  y: Float,\n}\n",
        "record Point {\n  x: Float,\n  y: Float,\n}\n",
    );
}

#[test]
fn format_record_with_default() {
    check(
        "record Config {\n  debug: Bool = false,\n  level: Int = 1,\n}\n",
        "record Config {\n  debug: Bool = false,\n  level: Int = 1,\n}\n",
    );
}

// ─── Enums ────────────────────────────────────────────────────────────────

#[test]
fn format_enum_unit_variants() {
    check(
        "enum Color {\n  Red,\n  Green,\n  Blue,\n}\n",
        "enum Color {\n  Red,\n  Green,\n  Blue,\n}\n",
    );
}

#[test]
fn format_enum_tuple_variant() {
    check(
        "enum Option[T] {\n  Some(T),\n  None,\n}\n",
        "enum Option[T] {\n  Some(T),\n  None,\n}\n",
    );
}

// ─── If/else ──────────────────────────────────────────────────────────────

#[test]
fn format_if_else() {
    // Bock requires parens around the condition (`if (cond)`, §21 grammar).
    check(
        "fn test() {\n  if (true) {\n    1\n  } else {\n    2\n  }\n}\n",
        "fn test() {\n  if (true) {\n    1\n  } else {\n    2\n  }\n}\n",
    );
}

// ─── Match ────────────────────────────────────────────────────────────────

#[test]
fn format_match() {
    check(
        "fn test(x: Int) {\n  match x {\n    1 => \"one\",\n    _ => \"other\",\n  }\n}\n",
        "fn test(x: Int) {\n  match x {\n    1 => \"one\",\n    _ => \"other\",\n  }\n}\n",
    );
}

// ─── Loops ────────────────────────────────────────────────────────────────

#[test]
fn format_for_loop() {
    check(
        "fn test() {\n  for x in items {\n    x\n  }\n}\n",
        "fn test() {\n  for x in items {\n    x\n  }\n}\n",
    );
}

#[test]
fn format_while_loop() {
    // `while (cond)` — parens required (§21 grammar).
    check(
        "fn test() {\n  while (true) {\n    break\n  }\n}\n",
        "fn test() {\n  while (true) {\n    break\n  }\n}\n",
    );
}

#[test]
fn format_loop_stmt() {
    check(
        "fn test() {\n  loop {\n    break\n  }\n}\n",
        "fn test() {\n  loop {\n    break\n  }\n}\n",
    );
}

// ─── Imports ──────────────────────────────────────────────────────────────

#[test]
fn format_import_sorting() {
    // Core before Std before local, with blank lines between groups
    check(
        "use Std.Io\nuse Core.Math\nuse mymod\n\nfn main() {}\n",
        "use Core.Math\n\nuse Std.Io\n\nuse mymod\n\nfn main() {}\n",
    );
}

#[test]
fn format_import_named() {
    check(
        "use Std.Collections.{ List, Map }\n\nfn main() {}\n",
        "use Std.Collections.{ List, Map }\n\nfn main() {}\n",
    );
}

// ─── Annotations ──────────────────────────────────────────────────────────

#[test]
fn format_annotation() {
    check(
        "@derive(Equatable)\nrecord Point {\n  x: Int,\n}\n",
        "@derive(Equatable)\nrecord Point {\n  x: Int,\n}\n",
    );
}

// ─── Visibility ───────────────────────────────────────────────────────────

#[test]
fn format_public_record() {
    check(
        "public record Point {\n  x: Int,\n}\n",
        "public record Point {\n  x: Int,\n}\n",
    );
}

// ─── Expression chains ───────────────────────────────────────────────────

#[test]
fn format_method_chain() {
    check(
        "fn test() {\n  x.foo().bar()\n}\n",
        "fn test() {\n  x.foo().bar()\n}\n",
    );
}

// ─── Binary expressions ──────────────────────────────────────────────────

#[test]
fn format_binary_expr() {
    check(
        "fn test() {\n  1 + 2 * 3\n}\n",
        "fn test() {\n  1 + 2 * 3\n}\n",
    );
}

// ─── Module path ──────────────────────────────────────────────────────────

#[test]
fn format_module_path() {
    check(
        "module Std.Io\n\nfn main() {}\n",
        "module Std.Io\n\nfn main() {}\n",
    );
}

// ─── Doc comments ─────────────────────────────────────────────────────────

#[test]
fn format_module_doc_comment() {
    check(
        "//! This is a module doc\n\nfn main() {}\n",
        "//! This is a module doc\n\nfn main() {}\n",
    );
}

// ─── Generics ─────────────────────────────────────────────────────────────

#[test]
fn format_generic_fn() {
    check(
        "fn identity[T](x: T) -> T {\n  x\n}\n",
        "fn identity[T](x: T) -> T {\n  x\n}\n",
    );
}

// NOTE: const and type alias tests are deferred — the parser doesn't yet
// support these constructs at the top level.

// ─── Empty body ───────────────────────────────────────────────────────────

#[test]
fn format_empty_fn() {
    check("fn noop() {}\n", "fn noop() {}\n");
}

// ─── Idempotency ──────────────────────────────────────────────────────────

#[test]
fn format_is_idempotent() {
    let src = "fn main() {\n  let x = 1 + 2\n  x\n}\n";
    let result1 = format_source(src, "test.bock");
    let result2 = format_source(&result1.output, "test.bock");
    assert_eq!(result1.output, result2.output);
    assert!(!result2.changed);
}

// ─── Check mode ───────────────────────────────────────────────────────────

#[test]
fn format_unchanged_reports_no_change() {
    let src = "fn main() {}\n";
    let result = format_source(src, "test.bock");
    assert!(!result.changed);
}

// ─── List literal ─────────────────────────────────────────────────────────

#[test]
fn format_list_literal() {
    check(
        "fn test() {\n  [1, 2, 3]\n}\n",
        "fn test() {\n  [1, 2, 3]\n}\n",
    );
}

// ─── Lambda ───────────────────────────────────────────────────────────────

#[test]
fn format_lambda() {
    check(
        "fn test() {\n  (x) => x + 1\n}\n",
        "fn test() {\n  (x) => x + 1\n}\n",
    );
}

// ─── Return/break/continue ────────────────────────────────────────────────

#[test]
fn format_return() {
    check(
        "fn test() {\n  return 42\n}\n",
        "fn test() {\n  return 42\n}\n",
    );
}

// ─── String interpolation ─────────────────────────────────────────────────

#[test]
fn format_interpolation() {
    check(
        "fn test(name: String) {\n  \"hello ${name}\"\n}\n",
        "fn test(name: String) {\n  \"hello ${name}\"\n}\n",
    );
}

// ─── Trait ────────────────────────────────────────────────────────────────

#[test]
fn format_trait() {
    check(
        "trait Printable {\n  fn print(self) -> String {\n    \"\"\n  }\n}\n",
        "trait Printable {\n  fn print(self) -> String {\n    \"\"\n  }\n}\n",
    );
}

// ─── Effect ───────────────────────────────────────────────────────────────

#[test]
fn format_effect() {
    check(
        "effect Log {\n  fn log(msg: String) {}\n}\n",
        "effect Log {\n  fn log(msg: String) {}\n}\n",
    );
}

// ─── Parse error passthrough ──────────────────────────────────────────────

#[test]
fn format_parse_error_returns_unchanged() {
    let bad = "fn {{{ broken";
    let result = format_source(bad, "test.bock");
    assert_eq!(result.output, bad);
    assert!(!result.changed);
}

// ─── Comments (best-effort) ──────────────────────────────────────────────

#[test]
fn format_preserves_line_comment() {
    // Line comments should be preserved (best-effort)
    let src = "// a comment\nfn main() {}\n";
    let result = format_source(src, "test.bock");
    assert!(
        result.output.contains("// a comment"),
        "Line comment should be preserved"
    );
}

// ─── Hard line limit (100 chars) ─────────────────────────────────────────

/// Helper: assert no output line exceeds 100 characters.
fn assert_no_line_exceeds_limit(output: &str) {
    for (i, line) in output.lines().enumerate() {
        assert!(
            line.len() <= 100,
            "Line {} exceeds 100 chars (len={}): {:?}",
            i + 1,
            line.len(),
            line
        );
    }
}

#[test]
fn hard_limit_long_function_signature() {
    // A function signature that exceeds 100 chars should be wrapped
    let src = "fn very_long_function_name(first_param: String, second_param: Int, third_param: Float, fourth_param: Bool) -> String {\n  \"ok\"\n}\n";
    let result = format_source(src, "test.bock");
    assert_no_line_exceeds_limit(&result.output);
    // Should still be valid (contains fn keyword and body)
    assert!(result.output.contains("fn very_long_function_name"));
    assert!(result.output.contains("\"ok\""));
}

#[test]
fn hard_limit_long_binary_expression() {
    // A long binary expression that exceeds 100 chars
    let src = "fn test() {\n  let result = first_value + second_value + third_value + fourth_value + fifth_value + sixth_value + seventh_value\n}\n";
    let result = format_source(src, "test.bock");
    assert_no_line_exceeds_limit(&result.output);
    assert!(result.output.contains("first_value"));
    assert!(result.output.contains("seventh_value"));
}

#[test]
fn hard_limit_long_method_chain() {
    // A long method chain that exceeds 100 chars — should break before "."
    let src = "fn test() {\n  something.first_method().second_method().third_method().fourth_method().fifth_method().sixth_method()\n}\n";
    let result = format_source(src, "test.bock");
    assert_no_line_exceeds_limit(&result.output);
    assert!(result.output.contains("something"));
    assert!(result.output.contains("sixth_method"));
}

#[test]
fn hard_limit_long_function_call_args() {
    // A long function call with many arguments
    let src = "fn test() {\n  some_function(first_argument, second_argument, third_argument, fourth_argument, fifth_argument, sixth_argument)\n}\n";
    let result = format_source(src, "test.bock");
    assert_no_line_exceeds_limit(&result.output);
    assert!(result.output.contains("some_function"));
    assert!(result.output.contains("sixth_argument"));
}

#[test]
fn hard_limit_preserves_short_lines() {
    // Lines under 100 chars should not be modified
    let src = "fn short() {\n  let x = 42\n}\n";
    let result = format_source(src, "test.bock");
    assert_eq!(result.output, src);
}

#[test]
fn hard_limit_idempotent() {
    // Wrapped output should be idempotent
    let src = "fn test() {\n  let result = first_value + second_value + third_value + fourth_value + fifth_value + sixth_value + seventh_value\n}\n";
    let result1 = format_source(src, "test.bock");
    assert_no_line_exceeds_limit(&result1.output);
    let result2 = format_source(&result1.output, "test.bock");
    assert_eq!(
        result1.output, result2.output,
        "Wrapping should be idempotent"
    );
}

#[test]
fn hard_limit_continuation_indent() {
    // Continuation lines should be indented by original + 4 spaces
    let src = "fn test() {\n  let result = first_value + second_value + third_value + fourth_value + fifth_value + sixth_value + seventh_value\n}\n";
    let result = format_source(src, "test.bock");
    let lines: Vec<&str> = result.output.lines().collect();
    // Find continuation lines (lines that are part of a wrapped statement)
    let mut found_continuation = false;
    for line in &lines {
        // Original indent is 2 spaces, continuation should be 2+4=6 spaces
        if line.starts_with("      ") && !line.starts_with("        ") {
            found_continuation = true;
        }
    }
    assert!(
        found_continuation,
        "Should have continuation lines with 6 spaces indent (2 base + 4 continuation)\nOutput:\n{}",
        result.output
    );
}

// ─── Visibility keyword (`public`, never `pub`) ────────────────────────────

#[test]
fn format_public_fn_preserves_keyword() {
    // Regression for Q-fmt-bock: the formatter used to rewrite `public` to the
    // Rust keyword `pub`, producing source that no longer parses.
    let out = format_source("public fn f() {\n  1\n}\n", "t.bock").output;
    assert!(
        out.contains("public fn f"),
        "expected `public fn`, got:\n{out}"
    );
    assert!(
        !out.contains("pub fn"),
        "must not emit Rust's `pub`:\n{out}"
    );
}

#[test]
fn format_public_keyword_on_all_item_kinds() {
    let src = "\
public fn f() {\n  1\n}\n\
\n\
public record R {\n  x: Int,\n}\n\
\n\
public enum E {\n  A,\n}\n\
\n\
public trait T {\n  fn m(self) -> Int\n}\n\
\n\
public const C: Int = 1\n";
    let out = format_source(src, "t.bock").output;
    assert_eq!(
        out.matches("public ").count(),
        5,
        "every item should keep `public`:\n{out}"
    );
    assert!(!out.contains("pub "), "no Rust `pub` anywhere:\n{out}");
    assert_parses(&out);
}

// ─── Doc comment preservation (`///`) ──────────────────────────────────────

#[test]
fn format_preserves_item_doc_comment() {
    // Regression for Q-fmt-bock: `///` doc comments on items used to be dropped.
    let src = "/// Adds one.\nfn inc(x: Int) -> Int {\n  x + 1\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(out.contains("/// Adds one."), "doc comment dropped:\n{out}");
    assert!(
        out.starts_with("/// Adds one.\nfn inc"),
        "doc comment must stay attached above its item:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_preserves_multiline_doc_comment() {
    let src = "/// Line one.\n/// Line two.\npublic fn f() {\n  1\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        out.contains("/// Line one."),
        "first doc line dropped:\n{out}"
    );
    assert!(
        out.contains("/// Line two."),
        "second doc line dropped:\n{out}"
    );
    assert!(out.contains("public fn f"), "public lost:\n{out}");
    assert_parses(&out);
}

#[test]
fn format_preserves_doc_on_trait_method() {
    // Doc comments on nested members (trait methods) were mis-placed: they got
    // swept into the *next* top-level item's leading comments.
    let src = "\
public trait T {\n  /// Does the thing.\n  fn m(self) -> Int\n}\n\
\n\
public record R {\n  x: Int,\n}\n";
    let out = format_source(src, "t.bock").output;
    // The doc must appear inside the trait, immediately above `fn m`.
    let idx_doc = out.find("/// Does the thing.").expect("doc dropped");
    let idx_m = out.find("fn m(self)").expect("method missing");
    let idx_record = out.find("record R").expect("record missing");
    assert!(idx_doc < idx_m, "doc must precede its method:\n{out}");
    assert!(
        idx_doc < idx_record,
        "doc must stay inside the trait, not before the record:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_preserves_doc_on_impl_method() {
    // Doc comments on impl methods used to land *inside* the method body.
    let src = "\
impl Error for SimpleError {\n  /// Returns the message.\n  public fn message(self) -> String {\n    self.msg\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    let idx_doc = out.find("/// Returns the message.").expect("doc dropped");
    let idx_fn = out.find("public fn message").expect("method missing");
    assert!(
        idx_doc < idx_fn,
        "doc must precede the method, not sit in its body:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_preserves_doc_on_record_field() {
    let src = "public record R {\n  /// The width.\n  width: Int,\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(out.contains("/// The width."), "field doc dropped:\n{out}");
    assert_parses(&out);
}

#[test]
fn format_preserves_doc_on_enum_variant() {
    let src = "public enum E {\n  /// The first.\n  A,\n  /// The second.\n  B,\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        out.contains("/// The first."),
        "variant doc dropped:\n{out}"
    );
    assert!(
        out.contains("/// The second."),
        "variant doc dropped:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_doc_comment_round_trip_is_idempotent() {
    let src = "/// Doc.\npublic fn f() {\n  1\n}\n";
    let first = format_source(src, "t.bock").output;
    let second = format_source(&first, "t.bock");
    assert_eq!(second.output, first, "fmt(fmt(x)) != fmt(x)");
    assert!(!second.changed, "second format should report no change");
}

// ─── Full stdlib fixture round-trip (Q-fmt-bock) ───────────────────────────

#[test]
fn format_error_bock_round_trips_to_valid_bock() {
    // The hand-authored `core.error` module exercises `//!`, `///`, and
    // `public` on a trait, record, impl method, and free function. Formatting
    // it must produce valid, parseable Bock that keeps every doc comment and
    // every `public`.
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../stdlib/core/error/error.bock"
    ))
    .expect("read error.bock");

    let out = format_source(&src, "error.bock").output;

    // Re-parses cleanly.
    assert_parses(&out);

    // No `pub` leaked in.
    assert!(
        !out.contains("pub "),
        "Rust `pub` leaked into output:\n{out}"
    );

    // Same number of `public` and `///` lines as the input — nothing dropped.
    let count = |hay: &str, needle: &str| hay.matches(needle).count();
    assert_eq!(
        count(&out, "public "),
        count(&src, "public "),
        "`public` count changed:\n{out}"
    );
    assert_eq!(
        count(&out, "///"),
        count(&src, "///"),
        "`///` doc-comment count changed:\n{out}"
    );

    // Idempotent.
    let out2 = format_source(&out, "error.bock").output;
    assert_eq!(out2, out, "error.bock formatting is not idempotent");
}

#[test]
fn format_public_use_preserves_visibility() {
    // Regression for Q-fmt-bock: `public use` re-exports must keep `public`.
    let out = format_source("public use core.error.{ Error }\n", "t.bock").output;
    assert!(
        out.starts_with("public use core.error"),
        "`public use` re-export lost its visibility:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_if_keeps_required_parens() {
    // §21 grammar: `if_expr = 'if' '(' condition ')' block`. The formatter must
    // not strip the parentheses, or the output stops parsing.
    let src = "fn f(x: Int) -> Int {\n  if (x < 0) {\n    0\n  } else {\n    x\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(out.contains("if (x < 0)"), "if parens stripped:\n{out}");
    assert!(out.contains("} else {"), "else mangled:\n{out}");
    assert_parses(&out);
}

#[test]
fn format_if_let_keeps_required_parens() {
    let src = "fn f(o: Int) -> Int {\n  if (let Some(v) = o) {\n    v\n  } else {\n    0\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        out.contains("if (let Some(v) = o)"),
        "if-let parens stripped:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_while_keeps_required_parens() {
    let src = "fn f() {\n  while (true) {\n    break\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        out.contains("while (true)"),
        "while parens stripped:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_guard_keeps_required_parens() {
    let src = "fn f(x: Int) -> Int {\n  guard (x > 0) else {\n    return 0\n  }\n  x\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        out.contains("guard (x > 0) else"),
        "guard parens stripped:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_match_guard_keeps_required_parens() {
    let src = "fn f(n: Int) -> Int {\n  match n {\n    x if (x > 100) => 1,\n    _ => 0,\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        out.contains("if (x > 100)"),
        "match-guard parens stripped:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_unit_variant_pattern_has_no_parens() {
    // A bare unit-variant pattern must stay bare (`Greater`), not gain empty
    // parens (`Greater()`).
    let src = "public enum E {\n  A,\n  B,\n}\n\nfn f(e: E) -> Int {\n  match e {\n    A => 1,\n    B => 2,\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        !out.contains("A()"),
        "unit pattern gained empty parens:\n{out}"
    );
    assert!(
        !out.contains("B()"),
        "unit pattern gained empty parens:\n{out}"
    );
    assert!(out.contains("A => 1"), "unit pattern arm mangled:\n{out}");
    assert_parses(&out);
}

#[test]
fn format_impl_preserves_trait_type_args() {
    // `impl From[Celsius] for Fahrenheit` — the trait's type arguments must be
    // preserved; dropping them changes which trait is implemented.
    let src = "impl From[Celsius] for Fahrenheit {\n  fn from(value: Celsius) -> Fahrenheit {\n    value\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        out.contains("impl From[Celsius] for Fahrenheit"),
        "trait type args dropped:\n{out}"
    );
    assert_parses(&out);
}

// ─── Control-flow match arms (Q-bockfmt-cfarm-comma) ──────────────────────

#[test]
fn format_cf_arm_break_no_trailing_comma() {
    // A value-less control-flow arm body (bare `break`) must NOT gain a trailing
    // comma: the parser reads the `,` as the start of `break`'s optional value
    // expression and rejects it with E2020 "expected expression, found `,`".
    // The formatted output must re-parse cleanly.
    let src =
        "fn f() {\n  loop {\n    match next() {\n      Some(x) => take(x),\n      None => break\n    }\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        !out.contains("break,"),
        "bare `break` arm gained an illegal trailing comma:\n{out}"
    );
    assert!(out.contains("None => break"), "break arm mangled:\n{out}");
    assert_parses(&out);
}

#[test]
fn format_cf_arm_continue_no_trailing_comma() {
    let src =
        "fn f() {\n  loop {\n    match next() {\n      Some(x) => take(x),\n      None => continue\n    }\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        !out.contains("continue,"),
        "`continue` arm gained an illegal trailing comma:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_cf_arm_return_bare_no_trailing_comma() {
    // A value-less `return` is illegal with a trailing comma, same as `break`.
    let src = "fn f() {\n  match opt() {\n    Some(x) => take(x),\n    None => return\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        !out.contains("return,"),
        "bare `return` arm gained an illegal trailing comma:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_cf_arm_return_value_keeps_trailing_comma() {
    // `return expr` (value-bearing) already absorbs the expression and stops at
    // the comma, so `=> return 0,` parses fine and KEEPS its trailing comma —
    // only the value-less form must drop it.
    let src = "fn f() -> Int {\n  match opt() {\n    Some(x) => x,\n    None => return 0\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        out.contains("return 0,"),
        "value-bearing `return` arm lost its trailing comma:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_cf_arm_break_value_keeps_trailing_comma() {
    // `break expr` (value-bearing) likewise keeps its trailing comma.
    let src =
        "fn f() -> Int {\n  loop {\n    match next() {\n      Some(x) => break x,\n      None => 0,\n    }\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(
        out.contains("break x,"),
        "value-bearing `break` arm lost its trailing comma:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_value_arm_keeps_trailing_comma() {
    // Regression guard: ordinary value arms must STILL get the trailing comma.
    let src = "fn f(n: Int) -> Int {\n  match n {\n    1 => 10,\n    _ => 0,\n  }\n}\n";
    let out = format_source(src, "t.bock").output;
    assert!(out.contains("1 => 10,"), "value arm lost its comma:\n{out}");
    assert!(out.contains("_ => 0,"), "value arm lost its comma:\n{out}");
    assert_parses(&out);
}

// ─── Long multi-byte comment lines (Q-bockfmt-utf8-panic) ──────────────────

#[test]
fn format_long_utf8_comment_no_panic() {
    // A box-drawing divider comment longer than the hard limit (100) whose
    // bytes do not align to char boundaries must NOT panic the line-wrapper.
    // Each `─` is 3 bytes; 90 of them is 270 bytes but only 90 chars, so a
    // naive byte-offset slice at 100 lands inside a multi-byte char.
    let divider: String = "─".repeat(90);
    let src = format!("// {divider}\nfn f() {{}}\n");
    // Must not panic.
    let out = format_source(&src, "t.bock").output;
    // The divider comment must be preserved intact (formatter leaves comments
    // alone; the wrapper must not corrupt or split mid-char).
    assert!(
        out.contains(&divider),
        "long multi-byte comment was corrupted:\n{out}"
    );
    assert_parses(&out);
}

#[test]
fn format_long_utf8_code_line_char_boundary() {
    // A long code line with multi-byte identifiers/strings must wrap on a char
    // boundary, never panicking and never producing invalid UTF-8 splits.
    let s: String = "α".repeat(80); // 80 chars, 160 bytes
    let src = format!("fn f() {{\n  let x = \"{s}\" + \"tail value that pushes this line well past the hard limit here\"\n}}\n");
    // Must not panic on the multi-byte slice.
    let _out = format_source(&src, "t.bock").output;
}

/// Helper: assert that `src` parses with no lexer or parser errors.
fn assert_parses(src: &str) {
    use bock_lexer::Lexer;
    use bock_parser::Parser;
    use bock_source::SourceFile;

    let file = SourceFile::new(
        bock_errors::FileId(0),
        std::path::PathBuf::from("fmt-out.bock"),
        src.to_string(),
    );
    let mut lexer = Lexer::new(&file);
    let tokens = lexer.tokenize();
    assert!(
        !lexer.diagnostics().has_errors(),
        "formatted output failed to lex:\n{src}"
    );
    let mut parser = Parser::new(tokens, &file);
    let _ = parser.parse_module();
    assert!(
        !parser.diagnostics().has_errors(),
        "formatted output failed to parse:\n{src}"
    );
}
