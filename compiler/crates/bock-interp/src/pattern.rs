//! Pure pattern matching: test a [`Value`] against an AST [`Pattern`], extract bindings.
//!
//! This module provides [`match_pattern`], a pure function that structurally
//! matches a runtime value against a pattern and returns extracted bindings on
//! success. Guards are **not** evaluated here — that is handled by the
//! match-expression evaluator which calls this function first.

use bock_ast::{Literal, Pattern, RecordPatternField};

use crate::value::{BockString, OrdF64, Value};

/// A single extracted binding: variable name → matched value.
pub type Binding = (String, Value);

/// Attempt to match `value` against `pattern`.
///
/// Returns extracted bindings on success, `None` on failure.
/// Does **not** evaluate guards — guard evaluation requires the expression
/// evaluator and is handled by the match-expression evaluator in `interp.rs`.
#[must_use]
pub fn match_pattern(value: &Value, pattern: &Pattern) -> Option<Vec<Binding>> {
    match pattern {
        Pattern::Wildcard { .. } | Pattern::Rest { .. } => Some(vec![]),

        Pattern::Bind { name, .. } => Some(vec![(name.name.clone(), value.clone())]),

        Pattern::MutBind { name, .. } => Some(vec![(name.name.clone(), value.clone())]),

        Pattern::Literal { lit, .. } => {
            if literal_matches(lit, value) {
                Some(vec![])
            } else {
                None
            }
        }

        Pattern::Constructor { path, fields, .. } => {
            let variant_name = path.segments.last().map(|s| s.name.as_str()).unwrap_or("");
            match_constructor(variant_name, fields, value)
        }

        Pattern::Record {
            path, fields, rest, ..
        } => match_record(path, fields, *rest, value),

        Pattern::Tuple { elems, .. } => match_tuple(elems, value),

        Pattern::List { elems, rest, .. } => match_list(elems, rest.as_deref(), value),

        Pattern::Or { alternatives, .. } => {
            for alt in alternatives {
                if let Some(bindings) = match_pattern(value, alt) {
                    return Some(bindings);
                }
            }
            None
        }

        Pattern::Range {
            lo, hi, inclusive, ..
        } => match_range(lo, hi, *inclusive, value),
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Check whether a literal pattern matches a value.
fn literal_matches(lit: &Literal, value: &Value) -> bool {
    match lit {
        Literal::Int(s) => {
            let (numeric, _) = bock_ast::strip_type_suffix(s);
            let clean = numeric.replace('_', "");
            let parsed = if clean.starts_with("0x") || clean.starts_with("0X") {
                i64::from_str_radix(&clean[2..], 16)
            } else if clean.starts_with("0o") || clean.starts_with("0O") {
                i64::from_str_radix(&clean[2..], 8)
            } else if clean.starts_with("0b") || clean.starts_with("0B") {
                i64::from_str_radix(&clean[2..], 2)
            } else {
                clean.parse::<i64>()
            };
            matches!(parsed, Ok(n) if *value == Value::Int(n))
        }
        Literal::Float(s) => {
            let (numeric, _) = bock_ast::strip_type_suffix(s);
            let parsed = numeric.replace('_', "").parse::<f64>();
            matches!(parsed, Ok(f) if *value == Value::Float(OrdF64(f)))
        }
        Literal::Bool(b) => *value == Value::Bool(*b),
        Literal::Char(s) => *value == Value::Char(s.chars().next().unwrap_or('\0')),
        Literal::String(s) => *value == Value::String(BockString::new(s.clone())),
        Literal::Unit => *value == Value::Void,
    }
}

/// Match a constructor pattern (Some, None, Ok, Err, or user-defined enum variants).
fn match_constructor(
    variant_name: &str,
    fields: &[Pattern],
    value: &Value,
) -> Option<Vec<Binding>> {
    match (variant_name, value) {
        ("Some", Value::Optional(Some(inner))) => {
            if fields.len() == 1 {
                match_pattern(inner, &fields[0])
            } else {
                None
            }
        }
        ("None", Value::Optional(None)) => {
            if fields.is_empty() {
                Some(vec![])
            } else {
                None
            }
        }
        ("Ok", Value::Result(Ok(inner))) => {
            if fields.is_empty() {
                Some(vec![])
            } else if fields.len() == 1 {
                match_pattern(inner, &fields[0])
            } else {
                None
            }
        }
        ("Err", Value::Result(Err(inner))) => {
            if fields.is_empty() {
                Some(vec![])
            } else if fields.len() == 1 {
                match_pattern(inner, &fields[0])
            } else {
                None
            }
        }
        (name, Value::Enum(ev)) if ev.variant == name => match (&ev.payload, fields.len()) {
            (None, 0) => Some(vec![]),
            (Some(inner), 1) => match_pattern(inner, &fields[0]),
            _ => None,
        },
        _ => None,
    }
}

/// Match a record pattern against a record value.
fn match_record(
    path: &bock_ast::TypePath,
    fields: &[RecordPatternField],
    rest: bool,
    value: &Value,
) -> Option<Vec<Binding>> {
    let rv = match value {
        Value::Record(rv) => rv,
        _ => return None,
    };

    let type_name = path.segments.last().map(|s| s.name.as_str()).unwrap_or("");
    if rv.type_name != type_name {
        return None;
    }
    // Without `..`, all fields must be covered.
    if !rest && fields.len() != rv.fields.len() {
        return None;
    }

    let mut bindings = Vec::new();
    for field in fields {
        let field_val = rv.fields.get(&field.name.name)?;
        if let Some(pat) = &field.pattern {
            bindings.extend(match_pattern(field_val, pat)?);
        } else {
            // Shorthand: `{ name }` ≡ `{ name: name }`
            bindings.push((field.name.name.clone(), field_val.clone()));
        }
    }
    Some(bindings)
}

/// Match a tuple pattern against a tuple value.
fn match_tuple(elems: &[Pattern], value: &Value) -> Option<Vec<Binding>> {
    let vals = match value {
        Value::Tuple(vals) => vals,
        _ => return None,
    };
    if elems.len() != vals.len() {
        return None;
    }
    let mut bindings = Vec::new();
    for (pat, val) in elems.iter().zip(vals.iter()) {
        bindings.extend(match_pattern(val, pat)?);
    }
    Some(bindings)
}

/// Match a list pattern against a list value.
fn match_list(elems: &[Pattern], rest: Option<&Pattern>, value: &Value) -> Option<Vec<Binding>> {
    let vals = match value {
        Value::List(vals) => vals,
        _ => return None,
    };
    if elems.len() > vals.len() {
        return None;
    }
    if rest.is_none() && elems.len() != vals.len() {
        return None;
    }
    let mut bindings = Vec::new();
    for (pat, val) in elems.iter().zip(vals.iter()) {
        bindings.extend(match_pattern(val, pat)?);
    }
    if let Some(rest_pat) = rest {
        let rest_val = Value::List(vals[elems.len()..].to_vec());
        bindings.extend(match_pattern(&rest_val, rest_pat)?);
    }
    Some(bindings)
}

/// Match a range pattern: both endpoints must be literal patterns.
fn match_range(lo: &Pattern, hi: &Pattern, inclusive: bool, value: &Value) -> Option<Vec<Binding>> {
    let lo_val = pattern_to_value(lo)?;
    let hi_val = pattern_to_value(hi)?;
    let in_range = if inclusive {
        *value >= lo_val && *value <= hi_val
    } else {
        *value >= lo_val && *value < hi_val
    };
    if in_range {
        Some(vec![])
    } else {
        None
    }
}

/// Extract a comparable value from a literal pattern (used for range endpoints).
fn pattern_to_value(pattern: &Pattern) -> Option<Value> {
    match pattern {
        Pattern::Literal { lit, .. } => literal_to_value(lit),
        _ => None,
    }
}

/// Convert a literal to a runtime value.
fn literal_to_value(lit: &Literal) -> Option<Value> {
    match lit {
        Literal::Int(s) => {
            let (numeric, _) = bock_ast::strip_type_suffix(s);
            let clean = numeric.replace('_', "");
            let n = if clean.starts_with("0x") || clean.starts_with("0X") {
                i64::from_str_radix(&clean[2..], 16)
            } else if clean.starts_with("0o") || clean.starts_with("0O") {
                i64::from_str_radix(&clean[2..], 8)
            } else if clean.starts_with("0b") || clean.starts_with("0B") {
                i64::from_str_radix(&clean[2..], 2)
            } else {
                clean.parse::<i64>()
            };
            n.ok().map(Value::Int)
        }
        Literal::Float(s) => {
            let (numeric, _) = bock_ast::strip_type_suffix(s);
            numeric
                .replace('_', "")
                .parse::<f64>()
                .ok()
                .map(|f| Value::Float(OrdF64(f)))
        }
        Literal::Bool(b) => Some(Value::Bool(*b)),
        Literal::Char(s) => Some(Value::Char(s.chars().next().unwrap_or('\0'))),
        Literal::String(s) => Some(Value::String(BockString::new(s.clone()))),
        Literal::Unit => Some(Value::Void),
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{EnumValue, RecordValue};
    use bock_air::NodeIdGen;
    use bock_ast::{Ident, TypePath};
    use bock_errors::Span;
    use std::collections::BTreeMap;

    fn span() -> Span {
        Span::dummy()
    }

    fn ident(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: span(),
        }
    }

    fn gen() -> NodeIdGen {
        NodeIdGen::new()
    }

    fn type_path(name: &str) -> TypePath {
        TypePath {
            segments: vec![ident(name)],
            span: span(),
        }
    }

    // ── Wildcard ─────────────────────────────────────────────────────────────

    #[test]
    fn wildcard_matches_anything() {
        let g = gen();
        let pat = Pattern::Wildcard {
            id: g.next(),
            span: span(),
        };
        assert_eq!(match_pattern(&Value::Int(42), &pat), Some(vec![]));
        assert_eq!(match_pattern(&Value::Bool(true), &pat), Some(vec![]));
        assert_eq!(match_pattern(&Value::Void, &pat), Some(vec![]));
    }

    // ── Bind ─────────────────────────────────────────────────────────────────

    #[test]
    fn bind_captures_value() {
        let g = gen();
        let pat = Pattern::Bind {
            id: g.next(),
            span: span(),
            name: ident("x"),
        };
        let result = match_pattern(&Value::Int(99), &pat);
        assert_eq!(result, Some(vec![("x".into(), Value::Int(99))]));
    }

    #[test]
    fn mut_bind_captures_value() {
        let g = gen();
        let pat = Pattern::MutBind {
            id: g.next(),
            span: span(),
            name: ident("y"),
        };
        let result = match_pattern(&Value::Bool(true), &pat);
        assert_eq!(result, Some(vec![("y".into(), Value::Bool(true))]));
    }

    // ── Literal ──────────────────────────────────────────────────────────────

    #[test]
    fn literal_int_match() {
        let g = gen();
        let pat = Pattern::Literal {
            id: g.next(),
            span: span(),
            lit: Literal::Int("42".to_string()),
        };
        assert_eq!(match_pattern(&Value::Int(42), &pat), Some(vec![]));
        assert_eq!(match_pattern(&Value::Int(99), &pat), None);
    }

    #[test]
    fn literal_bool_match() {
        let g = gen();
        let pat = Pattern::Literal {
            id: g.next(),
            span: span(),
            lit: Literal::Bool(true),
        };
        assert_eq!(match_pattern(&Value::Bool(true), &pat), Some(vec![]));
        assert_eq!(match_pattern(&Value::Bool(false), &pat), None);
    }

    #[test]
    fn literal_string_match() {
        let g = gen();
        let pat = Pattern::Literal {
            id: g.next(),
            span: span(),
            lit: Literal::String("hello".to_string()),
        };
        assert_eq!(
            match_pattern(&Value::String(BockString::new("hello")), &pat),
            Some(vec![])
        );
        assert_eq!(
            match_pattern(&Value::String(BockString::new("world")), &pat),
            None
        );
    }

    #[test]
    fn literal_float_match() {
        let g = gen();
        let pat = Pattern::Literal {
            id: g.next(),
            span: span(),
            lit: Literal::Float("3.14".to_string()),
        };
        assert_eq!(
            match_pattern(&Value::Float(OrdF64(3.14)), &pat),
            Some(vec![])
        );
    }

    #[test]
    fn literal_unit_match() {
        let g = gen();
        let pat = Pattern::Literal {
            id: g.next(),
            span: span(),
            lit: Literal::Unit,
        };
        assert_eq!(match_pattern(&Value::Void, &pat), Some(vec![]));
    }

    // ── Constructor: Some / None / Ok / Err ──────────────────────────────────

    #[test]
    fn constructor_some_match() {
        let g = gen();
        let inner_pat = Pattern::Bind {
            id: g.next(),
            span: span(),
            name: ident("x"),
        };
        let pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("Some"),
            fields: vec![inner_pat],
        };
        let value = Value::Optional(Some(Box::new(Value::Int(5))));
        let result = match_pattern(&value, &pat);
        assert_eq!(result, Some(vec![("x".into(), Value::Int(5))]));
    }

    #[test]
    fn constructor_none_match() {
        let g = gen();
        let pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("None"),
            fields: vec![],
        };
        assert_eq!(match_pattern(&Value::Optional(None), &pat), Some(vec![]));
        // Some doesn't match None pattern
        let some = Value::Optional(Some(Box::new(Value::Int(1))));
        assert_eq!(match_pattern(&some, &pat), None);
    }

    #[test]
    fn constructor_ok_match() {
        let g = gen();
        let inner = Pattern::Bind {
            id: g.next(),
            span: span(),
            name: ident("v"),
        };
        let pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("Ok"),
            fields: vec![inner],
        };
        let value = Value::Result(Ok(Box::new(Value::Int(42))));
        assert_eq!(
            match_pattern(&value, &pat),
            Some(vec![("v".into(), Value::Int(42))])
        );
    }

    #[test]
    fn constructor_err_match() {
        let g = gen();
        let inner = Pattern::Bind {
            id: g.next(),
            span: span(),
            name: ident("e"),
        };
        let pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("Err"),
            fields: vec![inner],
        };
        let value = Value::Result(Err(Box::new(Value::String(BockString::new("fail")))));
        assert_eq!(
            match_pattern(&value, &pat),
            Some(vec![("e".into(), Value::String(BockString::new("fail")))])
        );
    }

    #[test]
    fn constructor_enum_match() {
        let g = gen();
        let inner = Pattern::Bind {
            id: g.next(),
            span: span(),
            name: ident("r"),
        };
        let pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("Circle"),
            fields: vec![inner],
        };
        let value = Value::Enum(EnumValue {
            type_name: "Shape".into(),
            variant: "Circle".into(),
            payload: Some(Box::new(Value::Float(OrdF64(1.5)))),
        });
        assert_eq!(
            match_pattern(&value, &pat),
            Some(vec![("r".into(), Value::Float(OrdF64(1.5)))])
        );
    }

    #[test]
    fn constructor_enum_no_payload() {
        let g = gen();
        let pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("Red"),
            fields: vec![],
        };
        let value = Value::Enum(EnumValue {
            type_name: "Color".into(),
            variant: "Red".into(),
            payload: None,
        });
        assert_eq!(match_pattern(&value, &pat), Some(vec![]));
    }

    #[test]
    fn constructor_enum_wrong_variant() {
        let g = gen();
        let pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("Red"),
            fields: vec![],
        };
        let value = Value::Enum(EnumValue {
            type_name: "Color".into(),
            variant: "Blue".into(),
            payload: None,
        });
        assert_eq!(match_pattern(&value, &pat), None);
    }

    // ── Record ───────────────────────────────────────────────────────────────

    #[test]
    fn record_match_shorthand() {
        let g = gen();
        let pat = Pattern::Record {
            id: g.next(),
            span: span(),
            path: type_path("Point"),
            fields: vec![
                RecordPatternField {
                    span: span(),
                    name: ident("x"),
                    pattern: None,
                },
                RecordPatternField {
                    span: span(),
                    name: ident("y"),
                    pattern: None,
                },
            ],
            rest: false,
        };
        let mut fields = BTreeMap::new();
        fields.insert("x".to_string(), Value::Int(1));
        fields.insert("y".to_string(), Value::Int(2));
        let value = Value::Record(RecordValue {
            type_name: "Point".into(),
            fields,
        });
        let result = match_pattern(&value, &pat).unwrap();
        assert!(result.contains(&("x".into(), Value::Int(1))));
        assert!(result.contains(&("y".into(), Value::Int(2))));
    }

    #[test]
    fn record_match_with_sub_pattern() {
        let g = gen();
        let pat = Pattern::Record {
            id: g.next(),
            span: span(),
            path: type_path("User"),
            fields: vec![RecordPatternField {
                span: span(),
                name: ident("name"),
                pattern: Some(Pattern::Bind {
                    id: g.next(),
                    span: span(),
                    name: ident("n"),
                }),
            }],
            rest: true,
        };
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), Value::String(BockString::new("alice")));
        fields.insert("age".to_string(), Value::Int(30));
        let value = Value::Record(RecordValue {
            type_name: "User".into(),
            fields,
        });
        let result = match_pattern(&value, &pat);
        assert_eq!(
            result,
            Some(vec![("n".into(), Value::String(BockString::new("alice")))])
        );
    }

    #[test]
    fn record_wrong_type_name() {
        let g = gen();
        let pat = Pattern::Record {
            id: g.next(),
            span: span(),
            path: type_path("Point"),
            fields: vec![],
            rest: true,
        };
        let value = Value::Record(RecordValue {
            type_name: "Rect".into(),
            fields: BTreeMap::new(),
        });
        assert_eq!(match_pattern(&value, &pat), None);
    }

    #[test]
    fn record_rest_ignores_extra_fields() {
        let g = gen();
        let pat = Pattern::Record {
            id: g.next(),
            span: span(),
            path: type_path("Point"),
            fields: vec![RecordPatternField {
                span: span(),
                name: ident("x"),
                pattern: None,
            }],
            rest: true,
        };
        let mut fields = BTreeMap::new();
        fields.insert("x".to_string(), Value::Int(1));
        fields.insert("y".to_string(), Value::Int(2));
        fields.insert("z".to_string(), Value::Int(3));
        let value = Value::Record(RecordValue {
            type_name: "Point".into(),
            fields,
        });
        let result = match_pattern(&value, &pat);
        assert_eq!(result, Some(vec![("x".into(), Value::Int(1))]));
    }

    #[test]
    fn record_no_rest_field_count_mismatch() {
        let g = gen();
        let pat = Pattern::Record {
            id: g.next(),
            span: span(),
            path: type_path("Point"),
            fields: vec![RecordPatternField {
                span: span(),
                name: ident("x"),
                pattern: None,
            }],
            rest: false,
        };
        let mut fields = BTreeMap::new();
        fields.insert("x".to_string(), Value::Int(1));
        fields.insert("y".to_string(), Value::Int(2));
        let value = Value::Record(RecordValue {
            type_name: "Point".into(),
            fields,
        });
        assert_eq!(match_pattern(&value, &pat), None);
    }

    // ── Tuple ────────────────────────────────────────────────────────────────

    #[test]
    fn tuple_match() {
        let g = gen();
        let pat = Pattern::Tuple {
            id: g.next(),
            span: span(),
            elems: vec![
                Pattern::Bind {
                    id: g.next(),
                    span: span(),
                    name: ident("a"),
                },
                Pattern::Bind {
                    id: g.next(),
                    span: span(),
                    name: ident("b"),
                },
            ],
        };
        let value = Value::Tuple(vec![Value::Int(1), Value::Int(2)]);
        let result = match_pattern(&value, &pat);
        assert_eq!(
            result,
            Some(vec![
                ("a".into(), Value::Int(1)),
                ("b".into(), Value::Int(2))
            ])
        );
    }

    #[test]
    fn tuple_length_mismatch() {
        let g = gen();
        let pat = Pattern::Tuple {
            id: g.next(),
            span: span(),
            elems: vec![Pattern::Bind {
                id: g.next(),
                span: span(),
                name: ident("a"),
            }],
        };
        let value = Value::Tuple(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(match_pattern(&value, &pat), None);
    }

    // ── List ─────────────────────────────────────────────────────────────────

    #[test]
    fn list_exact_match() {
        let g = gen();
        let pat = Pattern::List {
            id: g.next(),
            span: span(),
            elems: vec![
                Pattern::Bind {
                    id: g.next(),
                    span: span(),
                    name: ident("a"),
                },
                Pattern::Bind {
                    id: g.next(),
                    span: span(),
                    name: ident("b"),
                },
            ],
            rest: None,
        };
        let value = Value::List(vec![Value::Int(10), Value::Int(20)]);
        let result = match_pattern(&value, &pat);
        assert_eq!(
            result,
            Some(vec![
                ("a".into(), Value::Int(10)),
                ("b".into(), Value::Int(20))
            ])
        );
    }

    #[test]
    fn list_with_rest_bind() {
        let g = gen();
        let pat = Pattern::List {
            id: g.next(),
            span: span(),
            elems: vec![Pattern::Bind {
                id: g.next(),
                span: span(),
                name: ident("head"),
            }],
            rest: Some(Box::new(Pattern::Bind {
                id: g.next(),
                span: span(),
                name: ident("tail"),
            })),
        };
        let value = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let result = match_pattern(&value, &pat);
        assert_eq!(
            result,
            Some(vec![
                ("head".into(), Value::Int(1)),
                (
                    "tail".into(),
                    Value::List(vec![Value::Int(2), Value::Int(3)])
                ),
            ])
        );
    }

    #[test]
    fn list_with_rest_wildcard() {
        let g = gen();
        let pat = Pattern::List {
            id: g.next(),
            span: span(),
            elems: vec![Pattern::Bind {
                id: g.next(),
                span: span(),
                name: ident("first"),
            }],
            rest: Some(Box::new(Pattern::Rest {
                id: g.next(),
                span: span(),
            })),
        };
        let value = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let result = match_pattern(&value, &pat);
        assert_eq!(result, Some(vec![("first".into(), Value::Int(1))]));
    }

    #[test]
    fn list_too_short() {
        let g = gen();
        let pat = Pattern::List {
            id: g.next(),
            span: span(),
            elems: vec![
                Pattern::Bind {
                    id: g.next(),
                    span: span(),
                    name: ident("a"),
                },
                Pattern::Bind {
                    id: g.next(),
                    span: span(),
                    name: ident("b"),
                },
            ],
            rest: None,
        };
        let value = Value::List(vec![Value::Int(1)]);
        assert_eq!(match_pattern(&value, &pat), None);
    }

    #[test]
    fn list_no_rest_length_mismatch() {
        let g = gen();
        let pat = Pattern::List {
            id: g.next(),
            span: span(),
            elems: vec![Pattern::Bind {
                id: g.next(),
                span: span(),
                name: ident("a"),
            }],
            rest: None,
        };
        let value = Value::List(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(match_pattern(&value, &pat), None);
    }

    // ── Or-pattern ───────────────────────────────────────────────────────────

    #[test]
    fn or_pattern_first_alternative() {
        let g = gen();
        let pat = Pattern::Or {
            id: g.next(),
            span: span(),
            alternatives: vec![
                Pattern::Literal {
                    id: g.next(),
                    span: span(),
                    lit: Literal::Int("1".into()),
                },
                Pattern::Literal {
                    id: g.next(),
                    span: span(),
                    lit: Literal::Int("2".into()),
                },
            ],
        };
        assert_eq!(match_pattern(&Value::Int(1), &pat), Some(vec![]));
    }

    #[test]
    fn or_pattern_second_alternative() {
        let g = gen();
        let pat = Pattern::Or {
            id: g.next(),
            span: span(),
            alternatives: vec![
                Pattern::Literal {
                    id: g.next(),
                    span: span(),
                    lit: Literal::Int("1".into()),
                },
                Pattern::Literal {
                    id: g.next(),
                    span: span(),
                    lit: Literal::Int("2".into()),
                },
            ],
        };
        assert_eq!(match_pattern(&Value::Int(2), &pat), Some(vec![]));
    }

    #[test]
    fn or_pattern_no_match() {
        let g = gen();
        let pat = Pattern::Or {
            id: g.next(),
            span: span(),
            alternatives: vec![
                Pattern::Literal {
                    id: g.next(),
                    span: span(),
                    lit: Literal::Int("1".into()),
                },
                Pattern::Literal {
                    id: g.next(),
                    span: span(),
                    lit: Literal::Int("2".into()),
                },
            ],
        };
        assert_eq!(match_pattern(&Value::Int(3), &pat), None);
    }

    // ── Range ────────────────────────────────────────────────────────────────

    #[test]
    fn range_exclusive() {
        let g = gen();
        let pat = Pattern::Range {
            id: g.next(),
            span: span(),
            lo: Box::new(Pattern::Literal {
                id: g.next(),
                span: span(),
                lit: Literal::Int("1".into()),
            }),
            hi: Box::new(Pattern::Literal {
                id: g.next(),
                span: span(),
                lit: Literal::Int("10".into()),
            }),
            inclusive: false,
        };
        assert_eq!(match_pattern(&Value::Int(1), &pat), Some(vec![]));
        assert_eq!(match_pattern(&Value::Int(5), &pat), Some(vec![]));
        assert_eq!(match_pattern(&Value::Int(10), &pat), None); // exclusive
        assert_eq!(match_pattern(&Value::Int(0), &pat), None);
    }

    #[test]
    fn range_inclusive() {
        let g = gen();
        let pat = Pattern::Range {
            id: g.next(),
            span: span(),
            lo: Box::new(Pattern::Literal {
                id: g.next(),
                span: span(),
                lit: Literal::Int("1".into()),
            }),
            hi: Box::new(Pattern::Literal {
                id: g.next(),
                span: span(),
                lit: Literal::Int("10".into()),
            }),
            inclusive: true,
        };
        assert_eq!(match_pattern(&Value::Int(10), &pat), Some(vec![]));
        assert_eq!(match_pattern(&Value::Int(11), &pat), None);
    }

    // ── Rest pattern ─────────────────────────────────────────────────────────

    #[test]
    fn rest_pattern_matches_anything() {
        let g = gen();
        let pat = Pattern::Rest {
            id: g.next(),
            span: span(),
        };
        assert_eq!(match_pattern(&Value::Int(42), &pat), Some(vec![]));
    }

    // ── Nested patterns ──────────────────────────────────────────────────────

    #[test]
    fn nested_some_ok_tuple() {
        // Some(Ok((a, b)))
        let g = gen();
        let tuple_pat = Pattern::Tuple {
            id: g.next(),
            span: span(),
            elems: vec![
                Pattern::Bind {
                    id: g.next(),
                    span: span(),
                    name: ident("a"),
                },
                Pattern::Bind {
                    id: g.next(),
                    span: span(),
                    name: ident("b"),
                },
            ],
        };
        let ok_pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("Ok"),
            fields: vec![tuple_pat],
        };
        let some_pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("Some"),
            fields: vec![ok_pat],
        };
        let value = Value::Optional(Some(Box::new(Value::Result(Ok(Box::new(Value::Tuple(
            vec![Value::Int(1), Value::Int(2)],
        )))))));
        let result = match_pattern(&value, &some_pat);
        assert_eq!(
            result,
            Some(vec![
                ("a".into(), Value::Int(1)),
                ("b".into(), Value::Int(2))
            ])
        );
    }

    #[test]
    fn nested_some_ok_mismatch() {
        // Some(Ok((a, b))) against Some(Err(...))
        let g = gen();
        let tuple_pat = Pattern::Tuple {
            id: g.next(),
            span: span(),
            elems: vec![
                Pattern::Bind {
                    id: g.next(),
                    span: span(),
                    name: ident("a"),
                },
                Pattern::Bind {
                    id: g.next(),
                    span: span(),
                    name: ident("b"),
                },
            ],
        };
        let ok_pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("Ok"),
            fields: vec![tuple_pat],
        };
        let some_pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("Some"),
            fields: vec![ok_pat],
        };
        let value = Value::Optional(Some(Box::new(Value::Result(Err(Box::new(Value::Int(99)))))));
        assert_eq!(match_pattern(&value, &some_pat), None);
    }

    #[test]
    fn nested_list_with_constructor() {
        // [Some(x), ..]
        let g = gen();
        let some_pat = Pattern::Constructor {
            id: g.next(),
            span: span(),
            path: type_path("Some"),
            fields: vec![Pattern::Bind {
                id: g.next(),
                span: span(),
                name: ident("x"),
            }],
        };
        let pat = Pattern::List {
            id: g.next(),
            span: span(),
            elems: vec![some_pat],
            rest: Some(Box::new(Pattern::Rest {
                id: g.next(),
                span: span(),
            })),
        };
        let value = Value::List(vec![
            Value::Optional(Some(Box::new(Value::Int(5)))),
            Value::Optional(None),
        ]);
        let result = match_pattern(&value, &pat);
        assert_eq!(result, Some(vec![("x".into(), Value::Int(5))]));
    }

    #[test]
    fn record_with_nested_constructor() {
        // Point { x: 0, y } matches record where x is literal 0
        let g = gen();
        let pat = Pattern::Record {
            id: g.next(),
            span: span(),
            path: type_path("Point"),
            fields: vec![
                RecordPatternField {
                    span: span(),
                    name: ident("x"),
                    pattern: Some(Pattern::Literal {
                        id: g.next(),
                        span: span(),
                        lit: Literal::Int("0".to_string()),
                    }),
                },
                RecordPatternField {
                    span: span(),
                    name: ident("y"),
                    pattern: None,
                },
            ],
            rest: false,
        };
        let mut fields = BTreeMap::new();
        fields.insert("x".to_string(), Value::Int(0));
        fields.insert("y".to_string(), Value::Int(5));
        let value = Value::Record(RecordValue {
            type_name: "Point".into(),
            fields,
        });
        let result = match_pattern(&value, &pat);
        assert_eq!(result, Some(vec![("y".into(), Value::Int(5))]));
    }

    #[test]
    fn record_with_nested_constructor_mismatch() {
        let g = gen();
        let pat = Pattern::Record {
            id: g.next(),
            span: span(),
            path: type_path("Point"),
            fields: vec![
                RecordPatternField {
                    span: span(),
                    name: ident("x"),
                    pattern: Some(Pattern::Literal {
                        id: g.next(),
                        span: span(),
                        lit: Literal::Int("0".to_string()),
                    }),
                },
                RecordPatternField {
                    span: span(),
                    name: ident("y"),
                    pattern: None,
                },
            ],
            rest: false,
        };
        let mut fields = BTreeMap::new();
        fields.insert("x".to_string(), Value::Int(1)); // not 0
        fields.insert("y".to_string(), Value::Int(5));
        let value = Value::Record(RecordValue {
            type_name: "Point".into(),
            fields,
        });
        assert_eq!(match_pattern(&value, &pat), None);
    }

    // ── Non-matching value types ─────────────────────────────────────────────

    #[test]
    fn tuple_pattern_against_non_tuple() {
        let g = gen();
        let pat = Pattern::Tuple {
            id: g.next(),
            span: span(),
            elems: vec![Pattern::Wildcard {
                id: g.next(),
                span: span(),
            }],
        };
        assert_eq!(match_pattern(&Value::Int(1), &pat), None);
    }

    #[test]
    fn list_pattern_against_non_list() {
        let g = gen();
        let pat = Pattern::List {
            id: g.next(),
            span: span(),
            elems: vec![],
            rest: None,
        };
        assert_eq!(match_pattern(&Value::Int(1), &pat), None);
    }

    #[test]
    fn record_pattern_against_non_record() {
        let g = gen();
        let pat = Pattern::Record {
            id: g.next(),
            span: span(),
            path: type_path("Foo"),
            fields: vec![],
            rest: true,
        };
        assert_eq!(match_pattern(&Value::Int(1), &pat), None);
    }
}
