# Spec Excerpt: Statements and Control Flow

## Let Bindings
```bock
let name = value
let mut counter = 0
let (x, y) = get_point()       // destructuring
let value: Type = expr          // explicit type
```

## Guard
```bock
guard (condition) else { return Err(...) }
```
The else block MUST diverge (return, break, continue, or Never).

## For Loops
```bock
for item in collection { ... }
for (i, item) in collection.enumerate() { ... }
```

## While Loops
```bock
while (condition) { ... }
```
Parentheses required around condition.

## Infinite Loop
```bock
loop { ... break }
loop { ... break value }        // loop can return value
```

## Handling Block
```bock
handling (Log with handler, Clock with mock) {
  code_using_effects()
}
```

## Block as Expression
```bock
let result = {
  let a = compute()
  let b = transform(a)
  a + b                         // last expression is return value
}
```

## Statement Termination
Newlines terminate. Semicolons optional for multiple per line.
