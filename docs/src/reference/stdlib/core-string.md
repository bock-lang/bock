# core.string

Free-function string utilities and a value-semantics `StringBuilder`,
complementing the built-in `String` methods.

Per §18.3 the *transformation* and *predicate* operations on a `String`
are built-in **methods** that lower to each target's native string op —
call these directly on the value:

```bock
s.to_upper()    s.trim()          s.contains(p)
s.starts_with(p)  s.ends_with(p)   s.replace(from, to)
s.split(sep)    s.len()           s.byte_len()      s.is_empty()
```

`String.len()` returns the count of Unicode scalar values (characters),
not bytes; use `s.byte_len()` for the byte count.

`core.string` ships only the utilities that **complement** that method
set — it does not re-expose case-mapping, trimming, or splitting (those
are built-in methods). Import what you need:

```bock
use core.string.{repeat, pad_left, pad_right, lines, is_blank}
use core.string.{StringBuilder, builder, append}
```

## Functions

### `repeat`

```bock
public fn repeat(s: String, n: Int) -> String
```

Returns `s` concatenated with itself `n` times. `repeat("ab", 3)` is
`"ababab"`; a non-positive `n` yields the empty string.

### `pad_left`

```bock
public fn pad_left(s: String, width: Int, fill: String) -> String
```

Left-pads `s` with `fill` until it is at least `width` scalars wide.
`pad_left("7", 4, "0")` is `"0007"`; an `s` already at least `width` wide
is returned unchanged. Width is measured in Unicode scalars (§18.3); a
multi-scalar `fill` may overshoot `width` by up to `fill.len() - 1`.

### `pad_right`

```bock
public fn pad_right(s: String, width: Int, fill: String) -> String
```

Right-pads `s` with `fill` until it is at least `width` scalars wide.
`pad_right("7", 4, ".")` is `"7..."`; the right-hand mirror of
[`pad_left`](#pad_left), with the same width semantics.

### `lines`

```bock
public fn lines(s: String) -> List[String]
```

Splits `s` into its newline-separated lines. `lines("a\nb\nc")` is
`["a", "b", "c"]`. A named convenience over the built-in `split` for the
common `"\n"` case (it does not strip a trailing empty line, matching
`split` exactly).

### `is_blank`

```bock
public fn is_blank(s: String) -> Bool
```

Returns `true` when `s` is empty after trimming surrounding whitespace.
`is_blank("   ")` is `true`, `is_blank(" x ")` is `false`. Distinct from
the bare built-in `s.is_empty()`, which is `false` for an all-whitespace
string.

## StringBuilder

A value-semantics accumulator for building a `String` piece by piece.
Accumulation is **value-semantics**: `append` returns a *new*
`StringBuilder` rather than mutating in place, so appends chain as
`append(append(builder(), "a"), "b")`.

The query methods are named `char_count` and `blank` (rather than the
conventional `len`/`is_empty`) so they do not collide with the built-in
collection-length/emptiness method lowering, which fires on any
receiver.

### `StringBuilder`

```bock
public record StringBuilder { buffer: String }
```

Construct an empty builder with [`builder`](#builder), grow it with
[`append`](#append) (which returns a new builder), and extract the
accumulated text with `render`. `buffer` holds the text accumulated so
far.

| Method | Returns | Description |
| ------ | ------- | ----------- |
| `render(self)` | `String` | The accumulated text. |
| `char_count(self)` | `Int` | Number of accumulated scalars. |
| `blank(self)` | `Bool` | `true` when nothing has been appended. |

### `builder`

```bock
public fn builder() -> StringBuilder
```

Constructs an empty `StringBuilder` — the ergonomic starting point:
`append(builder(), "a")`.

### `append`

```bock
public fn append(b: StringBuilder, s: String) -> StringBuilder
```

Returns a *new* `StringBuilder` whose buffer is `b`'s buffer followed by
`s`. Does not mutate `b` (value semantics), so appends chain as
`append(append(builder(), "a"), "b")`.

## Reserved for v1.x

- `Regex` (§18.3) — extended regular-expression support ships in the
  separate `std.regex` package, not in `core.string`.
- A `join(parts, sep)` helper is **not** provided: use the built-in
  `parts.join(sep)` method on a `List[String]`.
- `reverse(s)` is deferred — a correct cross-target string reverse needs
  per-character access, which is outside the proven v1 surface.
