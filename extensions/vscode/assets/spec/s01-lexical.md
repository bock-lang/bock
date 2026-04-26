# Spec Excerpt: Lexical Structure

## Encoding
UTF-8 source files.

## Whitespace + Line Handling
Newlines terminate statements. Continuation contexts (statement
continues to next line) when:
1. Line ends with binary operator
2. Line ends with comma
3. Line ends with opening delimiter (`(`, `[`, `{`)
4. Next line starts with `.`
5. Next line starts with `|>`
6. Next line starts with closing delimiter (`)`, `]`, `}`)
7. Explicit `\` continuation
8. Next line starts with `else`

Multiple statements per line separated by `;`.

## Comments
```
// line comment
/* block comment (nestable) */
/// doc comment
//! module doc comment
```

## Identifiers
Start with letter or `_`, continue with letter, digit, or `_`.
Type identifiers must start with uppercase.

## Keywords
`fn` `let` `mut` `const` `if` `else` `match` `for` `in` `while`
`loop` `break` `continue` `return` `guard` `with` `handling`
`handle` `record` `enum` `class` `trait` `impl` `self` `Self`
`module` `use` `public` `internal` `native` `async` `await`
`effect` `platform` `where` `type` `true` `false` `Ok` `Err`
`Some` `None` `property` `forall` `unreachable`

## Numeric Literals
- Decimal: `42`, `1_000_000`
- Hex: `0xFF`, `0XFF`
- Octal: `0o77`
- Binary: `0b1010`
- Float: `3.14`, `1.0e10`, `2.5E-3`
- Type suffix: `42_u8`, `3.14_f64` (underscore + TYPE_IDENT)

## String Literals
- Standard: `"hello ${expr}"` — escape sequences + interpolation
- Raw: `r"no ${interpolation}"` — no escapes
- Multi-line: `"""..."""` — with interpolation
- Raw multi-line: `r"""..."""`
- Escape sequences: `\n` `\r` `\t` `\\` `\"` `\'` `\0` `\$`
  `\u{hex}` (Unicode)
- Interpolation: `${expression}` inside non-raw strings
- Escaped dollar: `$$`

## Characters
`'a'`, `'\n'`, `'\u{1F600}'`

## Operators
Arithmetic: `+` `-` `*` `/` `%` `**`
Comparison: `==` `!=` `<` `>` `<=` `>=`
Logical: `&&` `||` `!`
Bitwise: `&` `|` `^` `~`
Assignment: `=` `+=` `-=` `*=` `/=` `%=`
Special: `|>` `>>` `=>` `->` `?` `..` `..=` `_` `#`

## Punctuation
`(` `)` `[` `]` `{` `}` `,` `:` `;` `@` `.`

## Operator Precedence (low → high)
1. Assignment (`= += -= *= /= %=`) — Right
2. Pipe (`|>`) — Left
3. Compose (`>>`) — Left
4. Range (`.. ..=`) — None
5. Logical OR (`||`) — Left
6. Logical AND (`&&`) — Left
7. Comparison (`== != < > <= >= is`) — None
8. Bitwise OR (`|`) — Left
9. Bitwise XOR (`^`) — Left
10. Bitwise AND (`&`) — Left
11. Additive (`+ -`) — Left
12. Multiplicative (`* / %`) — Left
13. Power (`**`) — Right
14. Unary (`- ! ~`) — Prefix
15. Postfix (`() [] . ? .method()`) — Left

## Bit Shifts
There are no infix shift operators. `>>` is reserved for function
composition (level 3). Use methods for bit shifting:
```bock
let composed = parse >> validate >> transform   // composition
let shifted = value.shift_right(2)              // right-shift
let masked = value.shift_left(4)                // left-shift
```
