# Spec Excerpt: Type System

## Primitives
`Int`, `Float`, `Bool`, `String`, `Char`, `Void`, `Never`.
Sized: `Int8`–`Int128`, `UInt8`–`UInt64`, `Float32`, `Float64`.
Also: `Byte`, `Bytes`, `BigInt`, `BigFloat`, `Decimal`.

No implicit numeric coercion. Explicit conversion methods:
`Int.to_float()`, `Float.to_int()` (truncates toward zero),
`to_string()` on all primitives.

## Compound Types
- `Optional[T]` (shorthand: `T?`) — `Some(T) | None`
- `Result[T, E]` — `Ok(T) | Err(E)`
- `List[T]`, `Map[K, V]`, `Set[T]`
- Tuples: `(A, B, C)` — fixed-size heterogeneous

## Function Types
```
Fn(Int, Int) -> Int
Fn(String) -> Void with Log     // with effects
```

## Generics
Square brackets: `List[T]`, `Map[K, V]`, `fn first[T](list: List[T])`

## Trait Bounds
```
fn serialize[T: Serializable](value: T) -> String
fn merge[A, B, C](l: A, r: B) -> C
  where (A: Into[C], B: Into[C], C: Combinable)
```

## Refined Types
```
type Email = String where (matches(r"^[^@]+@[^@]+\.[^@]+$"))
type Port = Int where (1 <= self <= 65535)
type NonEmpty[T] = List[T] where (len(self) > 0)
```

## Capability Types
```
@requires(Capability.Network, Capability.Storage)
```
Taxonomy: Network, Storage, Crypto, GPU, Camera, Microphone,
Location, Notifications, Bluetooth, Biometrics, Clipboard,
SystemProcess, FFI, Environment, Clock, Random.

## Flexible Types (Sketch Mode)
Wide types inferred, tracked structurally, narrowed by usage.

## Type Expression Grammar
```ebnf
type_expr = type_primary { type_postfix } ;
type_primary = type_path [ generic_args ]
             | '(' type_expr { ',' type_expr } ')'
             | '(' ')' | fn_type | 'Self' ;
type_postfix = '?' ;
generic_args = '[' type_expr { ',' type_expr } ']' ;
generic_params = '[' generic_param { ',' generic_param } ']' ;
generic_param = TYPE_IDENT [ ':' type_bound { '+' type_bound } ] ;
fn_type = 'Fn' '(' [ types ] ')' '->' type_expr [ effect_clause ] ;
```
