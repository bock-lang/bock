# Spec Excerpt: Declarations

## Functions
```bock
[visibility] [async] fn name[T](param: Type) -> Return
  with Effect1, Effect2
  where (T: Bound)
{ body }
```
Private by default. `public` / `internal`.

## Records (value types)
```bock
record Name { field: Type, field2: Type = default }
```

## Enums (ADTs)
```bock
enum Name {
  Variant1
  Variant2 { field: Type }
  Variant3(Type, Type)
}
```

## Classes (OOP/Multi mode)
```bock
class Name : Parent, Trait {
  field: Type
  fn method(self) -> T { ... }
}
```
Single inheritance, multiple trait impl.

## Traits
```bock
trait Name {
  fn required(self) -> T
  fn default_impl(self) -> T { ... }
  type AssociatedType [: Bound]
}
```

## Platform Traits
```bock
platform trait Name { fn op() -> T }
```
Require per-target implementations.

## Impl Blocks
```bock
impl Trait for Type { fn method(self) -> T { ... } }
impl Type { fn method(self) -> T { ... } }
```

Associated functions (no `self` parameter) are called via `Type.method()`:
```bock
impl Point {
  fn origin() -> Point { Point { x: 0, y: 0 } }
}
let p = Point.origin()
```

## Type Aliases
```bock
type Name = Type where (predicate)
```

## Constants
```bock
const NAME: Type = value
```

## Derive
```bock
@derive(Equatable, Hashable, ToJson, FromJson)
record User { ... }
```
