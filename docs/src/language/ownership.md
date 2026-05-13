# Ownership

Bock has a lightweight ownership system. The goal is to
generate correct code for every target — including manual-memory
targets like Rust and C++ — without forcing the user to write
lifetime annotations. The compiler tracks who owns each value
and rejects use-after-move; everything else is inferred.

## Core Rules

1. **Values are owned.** Every value has exactly one owner.
2. **Ownership transfers on assignment** (move semantics by
   default).
3. **Borrowing is implicit for reads.** Passing a value to a
   function that only reads it does not transfer ownership.
4. **Explicit `mut` for mutable bindings.** A binding that
   needs to be reassigned or mutably borrowed is declared
   `let mut`.
5. **No lifetime annotations.** The compiler infers them.

```bock
record Resource {
  name: String
  data: List[Int]
}

impl Resource {
  fn describe(self) -> String {
    "Resource(${self.name}, len=${self.data.len()})"
  }
}

fn main() {
  let r = Resource { name: "config", data: [10, 20, 30] }
  // r is the owner; describe() borrows implicitly
  println(r.describe())
  // r is still usable
  println(r.describe())
}
```

<!-- verify: bock-check -->

## Move Semantics

When a value is assigned to a new binding, ownership transfers.
The old binding becomes unusable. Touching it is a compile
error:

```bock
fn consume(xs: List[Int]) -> Int { xs.len() }

fn main() {
  let a = [1, 2, 3]
  let b = a         // ownership moves from a to b
  // Using a now is an error:
  // let c = consume(a)  // ERROR: use of moved variable `a`
  let c = consume(b)   // b is the live binding
  println("${c}")
}
```

<!-- verify: bock-check -->

The compiler reports use-after-move with an error pointing at
both the move site and the use site:

```
[E5001] use of moved variable `a`
  ┌─ src/main.bock:5:11
  │
4 │   let b = a
  │           ─ value moved here
5 │   let c = consume(a)
  │                   ^ use of moved variable `a`
```

## Implicit Borrow

A function that does not need to take ownership of its argument
implicitly borrows it. The caller keeps ownership and can
continue to use the value:

```bock
fn count(items: List[Int]) -> Int {
  items.len()
}

fn main() {
  let data = [10, 20, 30, 40, 50]

  // Both calls borrow `data`; ownership never leaves the caller.
  let n1 = count(data)
  let n2 = count(data)
  let n3 = data.len()

  println("${n1} ${n2} ${n3}")
}
```

<!-- verify: bock-check -->

Whether a function moves or borrows is inferred from how it
uses its argument. The user does not annotate borrow vs move at
the call site.

## Mutable Bindings

A binding that needs to change is declared `let mut`. Without
`mut`, the binding cannot be reassigned:

```bock
fn double_each(xs: List[Int]) -> List[Int] {
  xs.map((x) => x * 2)
}

fn main() {
  let mut total = 0
  for x in [1, 2, 3, 4] {
    total = total + x
  }

  let mut numbers = [1, 2, 3]
  numbers = double_each(numbers)

  println("total=${total} first=${numbers.get(0)}")
}
```

<!-- verify: bock-check -->

Mutating a record field requires `mut` on the binding. Records
are value types: when you mutate one, you are conceptually
producing a new value and rebinding it.

## Control Flow and Ownership

At branch join points (`if`/`else`, `match` arms), the compiler
merges ownership states from each branch. Branches that diverge
— ones that `return`, `break`, `continue`, or evaluate to
`Never` — are excluded from the merge. This makes the common
patterns work without false errors:

```bock
fn process(items: List[Int]) -> Result[Int, String] {
  guard (items.len() > 0) else {
    return Err("empty input")
  }
  // items is still owned here — the guard's `return` diverges,
  // so the merge doesn't mark items as moved.
  Ok(items.len())
}

fn main() {
  match process([1, 2, 3]) {
    Ok(n) => println("ok ${n}")
    Err(e) => println("err ${e}")
  }
}
```

<!-- verify: bock-check -->

For non-diverging branches, merging is conservative: if **any**
branch moves a variable, the variable is considered moved after
the join. Moving a variable inside a loop body is always an
error — a second iteration would use a moved value.

## Target Mapping

Bock's ownership model is target-agnostic: the same source
compiles to languages with very different memory strategies.
The compiler chooses an appropriate translation per target:

| Bock concept | Rust | GC targets (JS, Python, Go) | C++ |
|--------------|------|------------------------------|-----|
| Ownership | Direct | Ignored (GC reclaims) | `std::move` where useful |
| Implicit borrow | `&T` | Pass by value/reference | `const T&` |
| Mutable borrow | `&mut T` | Pointer / reference | `T&` |
| Move | Move semantics | Reassignment | `std::move` |

The user does not select a memory strategy in source code. The
target profile drives the codegen choice. A single Bock
function compiles to idiomatic Rust with explicit borrows on
one target and to plain JS on another.

## The `@managed` Escape Hatch

Some code — UI tree construction, graph algorithms with cycles,
prototype scripts — is easier to write when the compiler does
not track ownership at all. `@managed` opts the function out of
move analysis; the codegen treats it as if every value were
GC-managed, regardless of target. On non-GC targets, the
compiler reaches for an appropriate runtime (e.g., reference
counting in Rust via `Rc`/`Arc` from `core.memory`).

```bock
@managed
fn build_ui(label: String) -> String {
  "${label}"
}

fn main() {
  println(build_ui("hello"))
}
```

<!-- verify: bock-check -->

Use `@managed` sparingly. Most code does not need it. It is
intended for the small fraction of programs where ownership
analysis fights the natural shape of the code.

## What Ownership Buys

The ownership system in Bock is the price of admission for
compiling to manual-memory targets without runtime garbage
collection. The compiler proves at build time that no value is
used after it is moved, that no reference outlives its referent,
and that mutable access is exclusive. The proof is what makes
the transpiled Rust or C++ idiomatic and safe — and it costs
the user almost nothing in syntax because the rules are simple
and the compiler infers the rest.
