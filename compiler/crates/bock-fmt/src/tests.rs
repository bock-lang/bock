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
fn format_fn_pub_async() {
    check(
        "pub async fn fetch(url: String) -> String {\n  url\n}\n",
        "pub async fn fetch(url: String) -> String {\n  url\n}\n",
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
    check(
        "fn test() {\n  if true {\n    1\n  } else {\n    2\n  }\n}\n",
        "fn test() {\n  if true {\n    1\n  } else {\n    2\n  }\n}\n",
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
    check(
        "fn test() {\n  while true {\n    break\n  }\n}\n",
        "fn test() {\n  while true {\n    break\n  }\n}\n",
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
fn format_pub_record() {
    check(
        "pub record Point {\n  x: Int,\n}\n",
        "pub record Point {\n  x: Int,\n}\n",
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
