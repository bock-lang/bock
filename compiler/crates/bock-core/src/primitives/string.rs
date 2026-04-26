//! String primitive type methods and trait implementations.
//!
//! Full method suite: split, join, trim, pad, replace, contains, starts_with,
//! ends_with, format, repeat, reverse, chars, bytes, and regex operations.

use bock_interp::{BockString, BuiltinRegistry, RuntimeError, TypeTag, Value};
use regex::Regex;

/// Register all String methods and trait implementations.
pub fn register(registry: &mut BuiltinRegistry) {
    // ── Add trait (concatenation) ────────────────────────────────────────
    registry.register(TypeTag::String, "add", string_add);

    // ── Comparable trait ─────────────────────────────────────────────────
    registry.register(TypeTag::String, "compare", string_compare);

    // ── Equatable trait ──────────────────────────────────────────────────
    registry.register(TypeTag::String, "equals", string_equals);

    // ── Hashable trait ───────────────────────────────────────────────────
    registry.register(TypeTag::String, "hash_code", string_hash_code);

    // ── Displayable trait ────────────────────────────────────────────────
    registry.register(TypeTag::String, "display", string_display);

    // ── Type-specific methods ────────────────────────────────────────────
    registry.register(TypeTag::String, "contains", string_contains);
    registry.register(TypeTag::String, "starts_with", string_starts_with);
    registry.register(TypeTag::String, "ends_with", string_ends_with);
    registry.register(TypeTag::String, "to_upper", string_to_upper);
    registry.register(TypeTag::String, "to_lower", string_to_lower);
    registry.register(TypeTag::String, "trim", string_trim);
    registry.register(TypeTag::String, "split", string_split);
    registry.register(TypeTag::String, "char_at", string_char_at);
    registry.register(TypeTag::String, "substring", string_substring);
    registry.register(TypeTag::String, "slice", string_substring);
    registry.register(TypeTag::String, "replace", string_replace);
    registry.register(TypeTag::String, "is_empty", string_is_empty);
    registry.register(TypeTag::String, "len", string_len);
    registry.register(TypeTag::String, "byte_len", string_byte_len);
    registry.register(TypeTag::String, "chars", string_chars);
    registry.register(TypeTag::String, "repeat", string_repeat);
    registry.register(TypeTag::String, "index_of", string_index_of);
    registry.register(TypeTag::String, "trim_start", string_trim_start);
    registry.register(TypeTag::String, "trim_end", string_trim_end);
    registry.register(TypeTag::String, "pad_start", string_pad_start);
    registry.register(TypeTag::String, "pad_end", string_pad_end);
    registry.register(TypeTag::String, "reverse", string_reverse);
    registry.register(TypeTag::String, "bytes", string_bytes);
    registry.register(TypeTag::String, "join", string_join);
    registry.register(TypeTag::String, "format", string_format);

    // ── Regex methods ─────────────────────────────────────────────────────
    registry.register(TypeTag::String, "regex_match", string_regex_match);
    registry.register(TypeTag::String, "regex_find", string_regex_find);
    registry.register(TypeTag::String, "regex_replace", string_regex_replace);
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn expect_str<'a>(args: &'a [Value], pos: usize, method: &str) -> Result<&'a str, RuntimeError> {
    match args.get(pos) {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "String.{method} expects String, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

fn expect_int(args: &[Value], pos: usize, method: &str) -> Result<i64, RuntimeError> {
    match args.get(pos) {
        Some(Value::Int(v)) => Ok(*v),
        Some(other) => Err(RuntimeError::TypeError(format!(
            "String.{method} expects Int, got {other}"
        ))),
        None => Err(RuntimeError::ArityMismatch {
            expected: pos + 1,
            got: args.len(),
        }),
    }
}

// ─── Add (concatenation) ─────────────────────────────────────────────────────

fn string_add(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_str(args, 0, "add")?;
    let b = expect_str(args, 1, "add")?;
    Ok(Value::String(BockString::new(format!("{a}{b}"))))
}

// ─── Comparable ───────────────────────────────────────────────────────────────

fn string_compare(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_str(args, 0, "compare")?;
    let b = expect_str(args, 1, "compare")?;
    Ok(Value::Int(a.cmp(b) as i64))
}

// ─── Equatable ────────────────────────────────────────────────────────────────

fn string_equals(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_str(args, 0, "equals")?;
    let b = expect_str(args, 1, "equals")?;
    Ok(Value::Bool(a == b))
}

// ─── Hashable ─────────────────────────────────────────────────────────────────

fn string_hash_code(args: &[Value]) -> Result<Value, RuntimeError> {
    use std::hash::{Hash, Hasher};
    let a = expect_str(args, 0, "hash_code")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    a.hash(&mut hasher);
    Ok(Value::Int(hasher.finish() as i64))
}

// ─── Displayable ──────────────────────────────────────────────────────────────

/// For strings, display returns the string itself (identity).
fn string_display(args: &[Value]) -> Result<Value, RuntimeError> {
    let a = expect_str(args, 0, "display")?;
    Ok(Value::String(BockString::new(a)))
}

// ─── Type-specific methods ────────────────────────────────────────────────────

fn string_contains(args: &[Value]) -> Result<Value, RuntimeError> {
    let haystack = expect_str(args, 0, "contains")?;
    let needle = expect_str(args, 1, "contains")?;
    Ok(Value::Bool(haystack.contains(needle)))
}

fn string_starts_with(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "starts_with")?;
    let prefix = expect_str(args, 1, "starts_with")?;
    Ok(Value::Bool(s.starts_with(prefix)))
}

fn string_ends_with(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "ends_with")?;
    let suffix = expect_str(args, 1, "ends_with")?;
    Ok(Value::Bool(s.ends_with(suffix)))
}

fn string_to_upper(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "to_upper")?;
    Ok(Value::String(BockString::new(s.to_uppercase())))
}

fn string_to_lower(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "to_lower")?;
    Ok(Value::String(BockString::new(s.to_lowercase())))
}

fn string_trim(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "trim")?;
    Ok(Value::String(BockString::new(s.trim())))
}

fn string_split(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "split")?;
    let sep = expect_str(args, 1, "split")?;
    let parts: Vec<Value> = s
        .split(sep)
        .map(|p| Value::String(BockString::new(p)))
        .collect();
    Ok(Value::List(parts))
}

fn string_char_at(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "char_at")?;
    let idx = expect_int(args, 1, "char_at")?;
    if idx < 0 {
        return Ok(Value::Optional(None));
    }
    match s.chars().nth(idx as usize) {
        Some(c) => Ok(Value::Optional(Some(Box::new(Value::Char(c))))),
        None => Ok(Value::Optional(None)),
    }
}

fn string_substring(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "substring")?;
    let start = expect_int(args, 1, "substring")?;
    let end = expect_int(args, 2, "substring")?;
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len() as i64;
    let start = start.max(0) as usize;
    let end = end.clamp(0, len) as usize;
    if start >= end {
        return Ok(Value::String(BockString::new("")));
    }
    let result: String = chars[start..end].iter().collect();
    Ok(Value::String(BockString::new(result)))
}

fn string_replace(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "replace")?;
    let from = expect_str(args, 1, "replace")?;
    let to = expect_str(args, 2, "replace")?;
    Ok(Value::String(BockString::new(s.replace(from, to))))
}

fn string_is_empty(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "is_empty")?;
    Ok(Value::Bool(s.is_empty()))
}

/// Returns the number of Unicode scalar values (characters) in the string.
fn string_len(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "len")?;
    Ok(Value::Int(s.chars().count() as i64))
}

fn string_byte_len(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "byte_len")?;
    Ok(Value::Int(s.len() as i64))
}

fn string_chars(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "chars")?;
    let chars: Vec<Value> = s.chars().map(Value::Char).collect();
    Ok(Value::List(chars))
}

fn string_repeat(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "repeat")?;
    let n = expect_int(args, 1, "repeat")?;
    if n < 0 {
        return Err(RuntimeError::TypeError(
            "String.repeat count must be non-negative".to_string(),
        ));
    }
    Ok(Value::String(BockString::new(s.repeat(n as usize))))
}

fn string_index_of(args: &[Value]) -> Result<Value, RuntimeError> {
    let haystack = expect_str(args, 0, "index_of")?;
    let needle = expect_str(args, 1, "index_of")?;
    // Return character index, not byte index
    match haystack.find(needle) {
        Some(byte_idx) => {
            let char_idx = haystack[..byte_idx].chars().count() as i64;
            Ok(Value::Optional(Some(Box::new(Value::Int(char_idx)))))
        }
        None => Ok(Value::Optional(None)),
    }
}

fn string_trim_start(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "trim_start")?;
    Ok(Value::String(BockString::new(s.trim_start())))
}

fn string_trim_end(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "trim_end")?;
    Ok(Value::String(BockString::new(s.trim_end())))
}

/// `"hi".pad_start(5, " ")` → `"   hi"`
fn string_pad_start(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "pad_start")?;
    let target_len = expect_int(args, 1, "pad_start")? as usize;
    let pad_char = expect_str(args, 2, "pad_start")?;
    let char_len = s.chars().count();
    if char_len >= target_len || pad_char.is_empty() {
        return Ok(Value::String(BockString::new(s)));
    }
    let pad_chars: Vec<char> = pad_char.chars().collect();
    let needed = target_len - char_len;
    let mut prefix = String::with_capacity(needed);
    for i in 0..needed {
        prefix.push(pad_chars[i % pad_chars.len()]);
    }
    prefix.push_str(s);
    Ok(Value::String(BockString::new(prefix)))
}

/// `"hi".pad_end(5, " ")` → `"hi   "`
fn string_pad_end(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "pad_end")?;
    let target_len = expect_int(args, 1, "pad_end")? as usize;
    let pad_char = expect_str(args, 2, "pad_end")?;
    let char_len = s.chars().count();
    if char_len >= target_len || pad_char.is_empty() {
        return Ok(Value::String(BockString::new(s)));
    }
    let pad_chars: Vec<char> = pad_char.chars().collect();
    let needed = target_len - char_len;
    let mut result = String::from(s);
    for i in 0..needed {
        result.push(pad_chars[i % pad_chars.len()]);
    }
    Ok(Value::String(BockString::new(result)))
}

fn string_reverse(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "reverse")?;
    let reversed: String = s.chars().rev().collect();
    Ok(Value::String(BockString::new(reversed)))
}

/// Returns a list of `Value::Int` byte values.
fn string_bytes(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "bytes")?;
    let bytes: Vec<Value> = s.bytes().map(|b| Value::Int(b as i64)).collect();
    Ok(Value::List(bytes))
}

/// Static-style join: `separator.join(list_of_strings)`.
fn string_join(args: &[Value]) -> Result<Value, RuntimeError> {
    let sep = expect_str(args, 0, "join")?;
    let list = match args.get(1) {
        Some(Value::List(items)) => items,
        Some(other) => {
            return Err(RuntimeError::TypeError(format!(
                "String.join expects List, got {other}"
            )))
        }
        None => {
            return Err(RuntimeError::ArityMismatch {
                expected: 2,
                got: args.len(),
            })
        }
    };
    let parts: Result<Vec<&str>, RuntimeError> = list
        .iter()
        .map(|v| match v {
            Value::String(s) => Ok(s.as_str()),
            other => Err(RuntimeError::TypeError(format!(
                "String.join list elements must be Strings, got {other}"
            ))),
        })
        .collect();
    Ok(Value::String(BockString::new(parts?.join(sep))))
}

/// Simple positional format: `"Hello, {}!".format("world")` → `"Hello, world!"`.
fn string_format(args: &[Value]) -> Result<Value, RuntimeError> {
    let template = expect_str(args, 0, "format")?;
    let format_args = &args[1..];
    let mut result = String::with_capacity(template.len());
    let mut arg_idx = 0;
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            if chars.peek() == Some(&'}') {
                chars.next();
                if arg_idx < format_args.len() {
                    result.push_str(&format_args[arg_idx].to_string());
                    arg_idx += 1;
                } else {
                    result.push_str("{}");
                }
            } else {
                result.push(c);
            }
        } else {
            result.push(c);
        }
    }
    Ok(Value::String(BockString::new(result)))
}

// ─── Regex methods ────────────────────────────────────────────────────────────

fn compile_regex(pattern: &str) -> Result<Regex, RuntimeError> {
    Regex::new(pattern).map_err(|e| RuntimeError::TypeError(format!("invalid regex pattern: {e}")))
}

/// `"hello123".regex_match("\\d+")` → `true`
fn string_regex_match(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "regex_match")?;
    let pattern = expect_str(args, 1, "regex_match")?;
    let re = compile_regex(pattern)?;
    Ok(Value::Bool(re.is_match(s)))
}

/// `"hello123world".regex_find("\\d+")` → `Optional(Some("123"))`
/// Returns a list of all matches.
fn string_regex_find(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "regex_find")?;
    let pattern = expect_str(args, 1, "regex_find")?;
    let re = compile_regex(pattern)?;
    let matches: Vec<Value> = re
        .find_iter(s)
        .map(|m| Value::String(BockString::new(m.as_str())))
        .collect();
    Ok(Value::List(matches))
}

/// `"hello123world".regex_replace("\\d+", "NUM")` → `"helloNUMworld"`
fn string_regex_replace(args: &[Value]) -> Result<Value, RuntimeError> {
    let s = expect_str(args, 0, "regex_replace")?;
    let pattern = expect_str(args, 1, "regex_replace")?;
    let replacement = expect_str(args, 2, "regex_replace")?;
    let re = compile_regex(pattern)?;
    let result = re.replace_all(s, replacement);
    Ok(Value::String(BockString::new(result.into_owned())))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> BuiltinRegistry {
        let mut r = BuiltinRegistry::new();
        register(&mut r);
        r
    }

    fn s(v: &str) -> Value {
        Value::String(BockString::new(v))
    }

    #[test]
    fn add_concat() {
        let r = reg();
        let result = r.call(TypeTag::String, "add", &[s("hello"), s(" world")]);
        assert_eq!(result.unwrap().unwrap(), s("hello world"));
    }

    #[test]
    fn compare_less() {
        let r = reg();
        let result = r.call(TypeTag::String, "compare", &[s("a"), s("b")]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(-1));
    }

    #[test]
    fn equals_true() {
        let r = reg();
        let result = r.call(TypeTag::String, "equals", &[s("hi"), s("hi")]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn display_identity() {
        let r = reg();
        let result = r.call(TypeTag::String, "display", &[s("test")]);
        assert_eq!(result.unwrap().unwrap(), s("test"));
    }

    #[test]
    fn contains_true() {
        let r = reg();
        let result = r.call(TypeTag::String, "contains", &[s("hello world"), s("world")]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn contains_false() {
        let r = reg();
        let result = r.call(TypeTag::String, "contains", &[s("hello"), s("xyz")]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn starts_with_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "starts_with", &[s("hello"), s("hel")]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn ends_with_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "ends_with", &[s("hello"), s("llo")]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn to_upper_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "to_upper", &[s("hello")]);
        assert_eq!(result.unwrap().unwrap(), s("HELLO"));
    }

    #[test]
    fn to_lower_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "to_lower", &[s("HELLO")]);
        assert_eq!(result.unwrap().unwrap(), s("hello"));
    }

    #[test]
    fn trim_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "trim", &[s("  hello  ")]);
        assert_eq!(result.unwrap().unwrap(), s("hello"));
    }

    #[test]
    fn split_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "split", &[s("a,b,c"), s(",")]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::List(vec![s("a"), s("b"), s("c")])
        );
    }

    #[test]
    fn char_at_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "char_at", &[s("hello"), Value::Int(1)]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::Optional(Some(Box::new(Value::Char('e'))))
        );
    }

    #[test]
    fn char_at_out_of_bounds() {
        let r = reg();
        let result = r.call(TypeTag::String, "char_at", &[s("hi"), Value::Int(5)]);
        assert_eq!(result.unwrap().unwrap(), Value::Optional(None));
    }

    #[test]
    fn substring_ok() {
        let r = reg();
        let result = r.call(
            TypeTag::String,
            "substring",
            &[s("hello world"), Value::Int(0), Value::Int(5)],
        );
        assert_eq!(result.unwrap().unwrap(), s("hello"));
    }

    #[test]
    fn slice_alias_ok() {
        let r = reg();
        let result = r.call(
            TypeTag::String,
            "slice",
            &[s("hello world"), Value::Int(0), Value::Int(5)],
        );
        assert_eq!(result.unwrap().unwrap(), s("hello"));
    }

    #[test]
    fn replace_ok() {
        let r = reg();
        let result = r.call(
            TypeTag::String,
            "replace",
            &[s("hello world"), s("world"), s("bock")],
        );
        assert_eq!(result.unwrap().unwrap(), s("hello bock"));
    }

    #[test]
    fn is_empty_true() {
        let r = reg();
        let result = r.call(TypeTag::String, "is_empty", &[s("")]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn is_empty_false() {
        let r = reg();
        let result = r.call(TypeTag::String, "is_empty", &[s("x")]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    // ── len (character count) ─────────────────────────────────────────

    #[test]
    fn len_ascii() {
        let r = reg();
        let result = r.call(TypeTag::String, "len", &[s("hello")]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(5));
    }

    #[test]
    fn len_multibyte() {
        let r = reg();
        // "héllo" has 5 chars but 6 bytes (é is 2 bytes in UTF-8)
        let result = r.call(TypeTag::String, "len", &[s("héllo")]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(5));
    }

    #[test]
    fn len_emoji() {
        let r = reg();
        // 🎉 is 4 bytes but 1 character
        let result = r.call(TypeTag::String, "len", &[s("🎉")]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(1));
    }

    // ── byte_len ────────────────────────────────────────────────────────

    #[test]
    fn byte_len_unicode() {
        let r = reg();
        // "héllo" has 5 chars but 6 bytes (é is 2 bytes in UTF-8)
        let result = r.call(TypeTag::String, "byte_len", &[s("héllo")]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(6));
    }

    #[test]
    fn byte_len_emoji() {
        let r = reg();
        // 🎉 is 4 bytes in UTF-8
        let result = r.call(TypeTag::String, "byte_len", &[s("🎉")]);
        assert_eq!(result.unwrap().unwrap(), Value::Int(4));
    }

    #[test]
    fn chars_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "chars", &[s("hi")]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::List(vec![Value::Char('h'), Value::Char('i')])
        );
    }

    #[test]
    fn repeat_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "repeat", &[s("ab"), Value::Int(3)]);
        assert_eq!(result.unwrap().unwrap(), s("ababab"));
    }

    #[test]
    fn index_of_found() {
        let r = reg();
        let result = r.call(TypeTag::String, "index_of", &[s("hello"), s("ll")]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::Optional(Some(Box::new(Value::Int(2))))
        );
    }

    #[test]
    fn index_of_not_found() {
        let r = reg();
        let result = r.call(TypeTag::String, "index_of", &[s("hello"), s("xyz")]);
        assert_eq!(result.unwrap().unwrap(), Value::Optional(None));
    }

    #[test]
    fn hash_code_deterministic() {
        let r = reg();
        let h1 = r
            .call(TypeTag::String, "hash_code", &[s("test")])
            .unwrap()
            .unwrap();
        let h2 = r
            .call(TypeTag::String, "hash_code", &[s("test")])
            .unwrap()
            .unwrap();
        assert_eq!(h1, h2);
    }

    // ── New string methods (P6.6) ─────────────────────────────────────────

    #[test]
    fn trim_start_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "trim_start", &[s("  hello  ")]);
        assert_eq!(result.unwrap().unwrap(), s("hello  "));
    }

    #[test]
    fn trim_end_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "trim_end", &[s("  hello  ")]);
        assert_eq!(result.unwrap().unwrap(), s("  hello"));
    }

    #[test]
    fn pad_start_ok() {
        let r = reg();
        let result = r.call(
            TypeTag::String,
            "pad_start",
            &[s("hi"), Value::Int(5), s("0")],
        );
        assert_eq!(result.unwrap().unwrap(), s("000hi"));
    }

    #[test]
    fn pad_start_no_op_when_long_enough() {
        let r = reg();
        let result = r.call(
            TypeTag::String,
            "pad_start",
            &[s("hello"), Value::Int(3), s(" ")],
        );
        assert_eq!(result.unwrap().unwrap(), s("hello"));
    }

    #[test]
    fn pad_end_ok() {
        let r = reg();
        let result = r.call(
            TypeTag::String,
            "pad_end",
            &[s("hi"), Value::Int(5), s(".")],
        );
        assert_eq!(result.unwrap().unwrap(), s("hi..."));
    }

    #[test]
    fn reverse_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "reverse", &[s("hello")]);
        assert_eq!(result.unwrap().unwrap(), s("olleh"));
    }

    #[test]
    fn reverse_unicode() {
        let r = reg();
        let result = r.call(TypeTag::String, "reverse", &[s("héllo")]);
        assert_eq!(result.unwrap().unwrap(), s("olléh"));
    }

    #[test]
    fn bytes_ok() {
        let r = reg();
        let result = r.call(TypeTag::String, "bytes", &[s("AB")]);
        assert_eq!(
            result.unwrap().unwrap(),
            Value::List(vec![Value::Int(65), Value::Int(66)])
        );
    }

    #[test]
    fn join_ok() {
        let r = reg();
        let list = Value::List(vec![s("a"), s("b"), s("c")]);
        let result = r.call(TypeTag::String, "join", &[s(", "), list]);
        assert_eq!(result.unwrap().unwrap(), s("a, b, c"));
    }

    #[test]
    fn join_empty_list() {
        let r = reg();
        let list = Value::List(vec![]);
        let result = r.call(TypeTag::String, "join", &[s("-"), list]);
        assert_eq!(result.unwrap().unwrap(), s(""));
    }

    #[test]
    fn format_ok() {
        let r = reg();
        let result = r.call(
            TypeTag::String,
            "format",
            &[
                s("Hello, {}! You are {} years old."),
                s("Alice"),
                Value::Int(30),
            ],
        );
        assert_eq!(
            result.unwrap().unwrap(),
            s("Hello, Alice! You are 30 years old.")
        );
    }

    #[test]
    fn format_no_placeholders() {
        let r = reg();
        let result = r.call(TypeTag::String, "format", &[s("no placeholders")]);
        assert_eq!(result.unwrap().unwrap(), s("no placeholders"));
    }

    // ── Regex tests ───────────────────────────────────────────────────────

    #[test]
    fn regex_match_true() {
        let r = reg();
        let result = r.call(TypeTag::String, "regex_match", &[s("hello123"), s("\\d+")]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(true));
    }

    #[test]
    fn regex_match_false() {
        let r = reg();
        let result = r.call(TypeTag::String, "regex_match", &[s("hello"), s("\\d+")]);
        assert_eq!(result.unwrap().unwrap(), Value::Bool(false));
    }

    #[test]
    fn regex_find_all() {
        let r = reg();
        let result = r.call(
            TypeTag::String,
            "regex_find",
            &[s("abc123def456"), s("\\d+")],
        );
        assert_eq!(
            result.unwrap().unwrap(),
            Value::List(vec![s("123"), s("456")])
        );
    }

    #[test]
    fn regex_find_no_matches() {
        let r = reg();
        let result = r.call(TypeTag::String, "regex_find", &[s("hello"), s("\\d+")]);
        assert_eq!(result.unwrap().unwrap(), Value::List(vec![]));
    }

    #[test]
    fn regex_replace_ok() {
        let r = reg();
        let result = r.call(
            TypeTag::String,
            "regex_replace",
            &[s("hello 123 world 456"), s("\\d+"), s("NUM")],
        );
        assert_eq!(result.unwrap().unwrap(), s("hello NUM world NUM"));
    }

    #[test]
    fn regex_invalid_pattern() {
        let r = reg();
        let result = r.call(TypeTag::String, "regex_match", &[s("test"), s("[invalid")]);
        assert!(result.unwrap().is_err());
    }
}
