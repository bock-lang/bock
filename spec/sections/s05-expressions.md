# Spec Excerpt: Expressions

## Expression-Valued Control Flow
`if`, `match`, blocks return values.

## Pipe Operator
```bock
data |> parse |> validate |> serialize
headers |> add(request, _, "Content-Type")  // placeholder
```
Pipe always prepends the LHS as the first argument. It does not
evaluate the RHS independently. For closures returned by functions:
```bock
let scaler = scale_by(10.0)   // bind closure first
data |> scaler                 // then pipe into it
```

## Function Composition
```bock
let process = parse >> validate >> transform
```

## Lambda Expressions
```bock
(x) => x * 2                    // single expression
(a, b) => { let c = a + b; c }  // block body
```
Parentheses always required around params.

## Collection Literals
```bock
[1, 2, 3]                       // List
{"key": "value"}                 // Map
#{"a", "b"}                      // Set
("hello", 42)                    // Tuple
```

## Record Construction
```bock
User { id: gen_id(), name, ..defaults }
```
Shorthand: `name` = `name: name`. Spread: `..expr`.

## Ranges
`1..10` (exclusive), `1..=10` (inclusive), `(0..100).step(2)`

## String Interpolation
`"Hello, ${user.name}!"`

## Error Propagation
`expr?` — returns Err early if Result is Err.

## If Expression
```bock
let x = if (cond) { a } else { b }
if (let Some(v) = expr) { ... }     // if-let
```

## Match Expression
```bock
match val {
  Pattern => expr
  Pattern if (guard) => expr
  _ => default
}
```
