# Lexical Structure

This page covers the surface tokens a Bock source file is built
from: encoding, whitespace and line handling, comments,
identifiers, literals, operators, and delimiters. The
authoritative grammar lives in [Grammar](./grammar.md) and in
§3 / §21 of `spec/bock-spec.md`.

## Encoding

Bock source files are UTF-8 encoded. The compiler reads files
as UTF-8 and rejects ill-formed bytes.

## Whitespace and Line Handling

Whitespace is not significant for block structure — blocks are
delimited by `{` and `}`. Newlines terminate statements unless
a continuation context is active. A statement continues to the
next line when any of the following hold:

1. The current line ends with a binary operator.
2. The current line ends with a comma.
3. The current line ends with an opening delimiter (`(`, `[`,
   `{`).
4. The next line starts with a dot `.`.
5. The next line starts with a pipe `|>`.
6. The next line starts with a closing delimiter (`)`, `]`,
   `}`).
7. An explicit line continuation `\` is present.
8. The next line starts with `else`.

Multiple statements on one line are separated by `;`. This is
allowed but rare — most code uses one statement per line.

```bock
fn keep_positive(xs: List[Int]) -> List[Int] {
  xs.filter((x) => x > 0)
}

fn list_sum(xs: List[Int]) -> Int {
  xs.sum()
}

fn main() {
  let items = [1, -2, 3, -4, 5]
  let total = items
    |> keep_positive
    |> list_sum
  println("${total}")
}
```

<!-- verify: bock-check -->

The pipe at the start of the continuation line keeps the
statement together. The same pattern works for method chains
starting with `.`.

## Comments

Bock has four comment forms:

```bock
// Line comment — ignored by the compiler
/* Block comment — also ignored; may be nested */
/// Documentation comment — attached to the next declaration
//! Module-level documentation comment — attached to the file
```

Doc comments (`///` and `//!`) are parsed and surfaced by
tooling (`bock doc`, hover info in the LSP) but do not affect
type checking. Both `///` and `//!` accept Markdown.

```bock
//! Math helpers used across the calculator examples.

/// Computes `n * n`.
///
/// Total function. Overflow on `Int` follows the platform's
/// wrapping behaviour.
fn square(n: Int) -> Int {
  n * n
}
```

<!-- verify: bock-check -->

## Identifiers

Identifiers start with a letter or underscore and continue with
letters, digits, or underscores. Type identifiers must start
with an uppercase letter; everything else (functions,
variables, fields, modules) starts lowercase by convention. The
compiler enforces the case rule for types.

```bock
fn main() {
  let user_id = 42
  let _unused = "anything"
  let mixedCase123 = "ok"
  println("${user_id} ${mixedCase123}")
}
```

<!-- verify: bock-check -->

A leading underscore is a hint to readers and to lint that the
binding is intentionally unused; it does not change semantics.

### Reserved Keywords

The following words are reserved and cannot be used as
identifiers:

```
fn        let       mut       const     if        else
match     for       in        while     loop      break
continue  return    guard     with      handling  handle
record    enum      class     trait     impl      self
Self      module    use       public    internal  native
async     await     effect    platform  where     type
true      false     Ok        Err       Some      None
property  forall    unreachable
```

Some of these — `Ok`, `Err`, `Some`, `None` — are also valid in
expression position because they construct prelude variants.

## Literals

### Integer Literals

Integers may be written in decimal, hexadecimal, octal, or
binary, and may contain underscores as visual separators:

```bock
fn main() {
  let dec = 42
  let big = 1_000_000
  let hex = 0xFF
  let oct = 0o77
  let bin = 0b1010
  println("${dec} ${big} ${hex} ${oct} ${bin}")
}
```

<!-- verify: bock-check -->

An optional type suffix selects a specific sized integer type:

```bock
fn main() {
  let small: Int8 = 42_i8
  let large = 1000_i64
  println("${small} ${large}")
}
```

<!-- verify: bock-check -->

### Float Literals

Floats use a decimal point or scientific notation:

```bock
fn main() {
  let pi = 3.14
  let avo = 6.022e23
  let small = 2.5E-3
  println("${pi} ${avo} ${small}")
}
```

<!-- verify: bock-check -->

A type suffix selects `Float32` or `Float64`:

```bock
let f = 3.14_f64
```

There is no implicit numeric coercion between `Int` and
`Float`. Use the conversion methods on the values:

```bock
let n = 10
let avg = n.to_float() / 3.0
let i = (5.7).to_int()    // truncates toward zero
```

### Boolean Literals

`true` and `false` are the two values of `Bool`. They are
reserved keywords; they cannot be rebound as identifiers.

### Character Literals

`Char` values are single Unicode scalar values written between
single quotes:

```bock
fn main() {
  let letter = 'A'
  let newline = '\n'
  let tab = '\t'
  let backslash = '\\'
  println("got ${letter}")
}
```

<!-- verify: bock-check -->

Supported escape sequences: `\n` (newline), `\t` (tab), `\r`
(carriage return), `\\` (backslash), `\'` (single quote), `\"`
(double quote), `\0` (null), and `\u{XXXX}` for any Unicode
scalar by hexadecimal code point.

### String Literals

Strings are double-quoted UTF-8 text. Bock supports four string
forms.

**Plain strings:**

```bock
let s = "hello, world"
let escaped = "line one\nline two"
```

**Interpolated strings.** Any string can contain `${expr}`,
where `expr` is any Bock expression. The compiler evaluates the
expression and inserts its `Displayable` form:

```bock
fn main() {
  let name = "Bock"
  let version = "0.1.0"
  println("${name} v${version}")
}
```

<!-- verify: bock-check -->

**Raw strings** disable escape and interpolation processing.
The string content is taken verbatim:

```bock
fn main() {
  let regex_src = r"^[a-z]+@[a-z]+\.[a-z]+$"
  let no_interp = r"literal ${not_evaluated} here"
  println(regex_src)
  println(no_interp)
}
```

<!-- verify: bock-check -->

**Multi-line strings** use triple quotes and preserve newlines.
Interpolation works inside them:

```bock
fn main() {
  let name = "World"
  let html = """
    <div>Hello, ${name}!</div>
  """
  println(html)
}
```

<!-- verify: bock-check -->

A raw multi-line string combines both forms:
`r""" ... """`. No escape processing, no interpolation,
newlines preserved.

## Operators

Operators are the same lexical tokens used in expressions; the
full precedence table is in [Expressions](./expressions.md).
The categories:

| Category | Operators |
|----------|-----------|
| Arithmetic | `+` `-` `*` `/` `%` `**` |
| Comparison | `==` `!=` `<` `>` `<=` `>=` `is` |
| Logical | `&&` `\|\|` `!` |
| Bitwise | `&` `\|` `^` `~` |
| Assignment | `=` `+=` `-=` `*=` `/=` `%=` |
| Special | `\|>` `>>` `=>` `->` `?` `..` `..=` `_` |

The `**` operator is exponentiation. There are no infix shift
operators — `>>` is reserved for function composition. Use
the `Int.shift_left(n)` and `Int.shift_right(n)` methods on
integers for shift operations.

## Delimiters

| Delimiter | Purpose |
|-----------|---------|
| `(` `)` | Function arguments, conditions, tuples, grouping |
| `[` `]` | Generic parameters, list literals, generic args |
| `{` `}` | Blocks, map literals, record construction |
| `#{` `}` | Set literals |
| `"` `"` | Strings |
| `"""` `"""` | Multi-line strings |
| `` ` `` | FFI inline code (inside `native` blocks) |

The shape of a `{ ... }` is disambiguated by what comes before
it: a type identifier turns it into a record construction
(`Point { x: 0, y: 0 }`); a leading `expression : expression`
makes it a map literal (`{"k": "v"}`); otherwise it is a
statement block.
