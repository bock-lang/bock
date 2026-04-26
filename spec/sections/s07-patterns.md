# Spec Excerpt: Pattern Matching

## Pattern Syntax
```ebnf
pattern = pattern_alt { '|' pattern_alt } ;
pattern_alt = '_' | IDENT | 'mut' IDENT | literal
            | type_path [ pattern_fields ]
            | '(' pattern { ',' pattern } ')'
            | '[' pattern { ',' pattern } ']'
            | pattern '..' pattern | '..' ;
```

## Pattern Types
- Wildcard: `_`
- Binding: `name` (binds matched value)
- Mutable binding: `mut name`
- Literal: `42`, `"hello"`, `true`
- Constructor: `Some(x)`, `Err(e)`
- Record: `Point { x, y }`, `User { name: n, .. }`
- Tuple: `(a, b, c)`
- List: `[first, second, ..]`
- Or-pattern: `A | B | C`
- Guard: `pattern if (condition)`
- Range: `1..10`
- Rest: `..` (ignore remaining fields/elements)

## Match Expression
```bock
match value {
  0 => "zero"
  1 | 2 => "small"
  n if (n > 100) => "large: ${n}"
  Point { x: 0, y } => "on y-axis at ${y}"
  Some(Ok(v)) => "got ${v}"
  [first, ..rest] => "head: ${first}"
  _ => "other"
}
```

## Exhaustiveness
Warned in `development`, enforced in `production`.
Match must cover all variants of an enum.

## If-Let
```bock
if (let Some(user) = find(id)) { use(user) }
```
Binding scoped to the if-block.
