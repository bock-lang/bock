# Grammar Reference

This page reproduces the formal EBNF grammar from §21 of
`spec/bock-spec.md`, organised by topic with a worked example
for each production cluster. The grammar is the authoritative
description of what the parser accepts; this page is a reading
aid.

## Notation

Productions use the following conventions:

- `UPPER_CASE` — terminal tokens defined by the lexer
  (`IDENT`, `TYPE_IDENT`, `STRING_LITERAL`, `NEWLINE`, etc.).
- `lower_case` — non-terminal productions.
- `'literal'` — a literal keyword or symbol.
- `[ x ]` — zero or one of `x`.
- `{ x }` — zero or more of `x`.
- `x | y` — alternation.

## Module Structure

```ebnf
source_file = { module_doc_comment } [ module_decl ]
              { import_decl } { top_level_item } ;
module_decl = 'module' module_path NEWLINE ;
module_path = IDENT { '.' IDENT } ;
import_decl = 'use' module_path [ import_list ] NEWLINE ;
import_list = '.' '{' IDENT { ',' IDENT } [ ',' ] '}'
            | '.' IDENT | '.' '*' ;
```

```bock
module utils.strings

use math.{double_it}

public fn shout(s: String) -> String { "${s}!" }
```

A source file optionally begins with module-level documentation
comments (`//!`), then an optional `module` declaration, then
imports, then top-level items. See [Modules](./modules.md) for
the resolution semantics.

## Top-Level Items

```ebnf
top_level_item = { annotation }
    ( fn_decl | record_decl | enum_decl | class_decl
    | trait_decl | platform_trait_decl | impl_block
    | effect_decl | type_alias | const_decl
    | module_handle_decl | property_test_decl ) ;
```

Any top-level item may be preceded by one or more annotations.

## Annotations

```ebnf
annotation = '@' annotation_name [ '(' annotation_arg_list ')' ] ;
annotation_name = IDENT { '.' IDENT } ;
annotation_arg_list = annotation_arg { ',' annotation_arg } [ ',' ] ;
annotation_arg = expression | IDENT ':' expression
               | STRING_LITERAL | MULTILINE_STRING ;
```

```bock
@context("Validates a card number.")
@requires(Capability.Network)
@performance(max_latency: 100)
fn validate(card: String) -> Bool { card.len() > 0 }

fn main() { println("${validate(\"4111\")}") }
```

Annotations are uniform across the language — the same syntax
applies to compiler directives (`@managed`, `@inline`),
capabilities (`@requires`), context annotations, derive macros,
and testing markers. See [Context](./context.md) for the
common annotation set.

## Functions

```ebnf
fn_decl = [ visibility ] [ 'async' ] 'fn' IDENT
          [ generic_params ] '(' [ param_list ] ')'
          [ '->' type_expr ] [ effect_clause ]
          [ where_clause ] block ;
visibility = 'public' | 'internal' ;
param_list = param { ',' param } [ ',' ] ;
param = [ 'mut' ] ( 'self' | IDENT ':' type_expr [ '=' expression ] ) ;
effect_clause = 'with' type_path { ',' type_path } ;
where_clause = 'where' '(' type_constraint { ',' type_constraint } [ ',' ] ')' ;
type_constraint = TYPE_IDENT ':' type_bound { '+' type_bound } ;
```

```bock
public fn merge[T](a: T, b: T) -> T
  with Log
  where (T: Comparable)
{
  if (a < b) { a } else { b }
}

fn main() { println("declared") }
```

See [Declarations](./declarations.md) for full coverage of
the `fn` form.

## Type Declarations

```ebnf
record_decl = [ visibility ] 'record' TYPE_IDENT [ generic_params ]
              [ where_clause ] '{' { record_field } '}' ;
record_field = { annotation } [ visibility ] IDENT ':' type_expr
               [ '=' expression ] NEWLINE ;
enum_decl = [ visibility ] 'enum' TYPE_IDENT [ generic_params ]
            [ where_clause ] '{' enum_variant { NEWLINE enum_variant } '}' ;
enum_variant = { annotation } TYPE_IDENT [ enum_variant_body ] ;
enum_variant_body = '{' record_field { record_field } '}'
                  | '(' type_expr { ',' type_expr } ')' ;
class_decl = [ visibility ] 'class' TYPE_IDENT [ generic_params ]
             [ ':' type_expr { ',' type_expr } ]
             [ where_clause ] '{' { class_member } '}' ;
trait_decl = [ visibility ] 'trait' TYPE_IDENT [ generic_params ]
             [ ':' type_bound { '+' type_bound } ]
             [ where_clause ] '{' { trait_member } '}' ;
platform_trait_decl = [ visibility ] 'platform' 'trait' TYPE_IDENT
                      [ generic_params ] [ where_clause ]
                      '{' { trait_member } '}' ;
impl_block = 'impl' [ generic_params ]
             [ type_path [ generic_args ] 'for' ]
             type_path [ generic_args ]
             [ where_clause ] '{' { fn_decl } '}' ;
```

```bock
record User {
  id: Int
  name: String
}

enum Status {
  Active,
  Pending { since: Int },
  Failed(String)
}

trait Show {
  fn show(self) -> String
}

impl Show for User {
  fn show(self) -> String { self.name }
}

fn main() {
  let u = User { id: 1, name: "Alice" }
  println(u.show())
}
```

See [Types](./types.md) and [Declarations](./declarations.md)
for the full surface.

## Effects

```ebnf
effect_decl = [ visibility ] 'effect' TYPE_IDENT
              [ '=' type_path { '+' type_path } ]
              '{' { fn_signature } '}' ;
            | [ visibility ] 'effect' TYPE_IDENT
              '=' type_path { '+' type_path } NEWLINE ;
fn_signature = [ visibility ] [ 'async' ] 'fn' IDENT
               [ generic_params ] '(' [ param_list ] ')'
               [ '->' type_expr ] [ effect_clause ]
               [ where_clause ] NEWLINE ;
```

```bock
effect Logger {
  fn log(msg: String) -> Void
}

effect Clock {
  fn now() -> Int
}

effect AppEffects = Logger + Clock

fn main() { println("hi") }
```

The composite-effect form on the second alternative declares a
union without operations. See [Effects](./effects.md).

## Other Declarations

```ebnf
type_alias = [ visibility ] 'type' TYPE_IDENT [ generic_params ]
             '=' type_expr [ 'where' '(' refinement_predicate ')' ] NEWLINE ;
const_decl = [ visibility ] 'const' IDENT ':' type_expr '=' expression NEWLINE ;
module_handle_decl = 'handle' type_path 'with' expression NEWLINE ;
```

```bock
module main

effect Logger {
  fn log(msg: String) -> Void
}

record ConsoleLogger {}

impl Logger for ConsoleLogger {
  fn log(msg: String) -> Void { println("[LOG] ${msg}") }
}

handle Logger with ConsoleLogger {}

const MAX_RETRIES: Int = 5
type UserId = String

fn main() { println("hi") }
```

The refinement predicate on `type_alias` is reserved syntax —
see the note in [Types](./types.md#refinement-types-spec).

## Type Expressions

```ebnf
type_expr = type_primary { type_postfix } ;
type_primary = type_path [ generic_args ]
             | '(' type_expr { ',' type_expr } ')'
             | '(' ')' | fn_type | 'Self' ;
type_postfix = '?' ;
type_path = TYPE_IDENT { '.' TYPE_IDENT }
          | module_path '.' TYPE_IDENT ;
generic_args = '[' type_expr { ',' type_expr } [ ',' ] ']' ;
generic_params = '[' generic_param { ',' generic_param } [ ',' ] ']' ;
generic_param = TYPE_IDENT [ ':' type_bound { '+' type_bound } ] ;
fn_type = 'Fn' '(' [ type_expr { ',' type_expr } ] ')'
          '->' type_expr [ effect_clause ] ;
```

`Int`, `List[T]`, `Result[Int, String]`, `(Int, String)`, `()`,
`Fn(Int) -> Int`, and `Optional[T]?` (an optional optional —
rare but legal) are all type expressions. The postfix `?`
converts any type into its optional form: `Int?` is equivalent
to `Optional[Int]`.

## Expressions

```ebnf
expression = assignment_expr ;
assignment_expr = pipe_expr [ assignment_op pipe_expr ] ;
pipe_expr = compose_expr { '|>' compose_expr } ;
compose_expr = range_expr { '>>' range_expr } ;
range_expr = or_expr [ ( '..' | '..=' ) or_expr ]
           | '..' [ or_expr ] | '..=' or_expr ;
or_expr = and_expr { '||' and_expr } ;
and_expr = comparison_expr { '&&' comparison_expr } ;
comparison_expr = bitwise_or_expr
                  { ( '==' | '!=' | '<' | '>' | '<=' | '>=' | 'is' )
                    bitwise_or_expr } ;
bitwise_or_expr = bitwise_xor_expr { '|' bitwise_xor_expr } ;
bitwise_xor_expr = bitwise_and_expr { '^' bitwise_and_expr } ;
bitwise_and_expr = additive_expr { '&' additive_expr } ;
additive_expr = multiplicative_expr { ( '+' | '-' ) multiplicative_expr } ;
multiplicative_expr = power_expr { ( '*' | '/' | '%' ) power_expr } ;
power_expr = unary_expr [ '**' power_expr ] ;
unary_expr = ( '-' | '!' | '~' ) unary_expr | postfix_expr ;
postfix_expr = primary_expr { postfix_op } ;
postfix_op = '(' [ arg_list ] ')' | '[' expression ']'
           | '.' IDENT | '.' IDENT '(' [ arg_list ] ')' | '?' ;
```

Precedence flows from low to high. See
[Expressions](./expressions.md) for the precedence table
expressed as a single chart and worked examples for each level.

## Primary Expressions

```ebnf
primary_expr = IDENT | TYPE_IDENT | literal | '(' expression ')'
             | '(' expression ',' expression { ',' expression } [ ',' ] ')'
             | if_expr | match_expr | block | lambda_expr
             | collection_literal | record_construction
             | 'await' expression | 'return' [ expression ]
             | 'break' [ expression ] | 'continue'
             | 'unreachable' '(' ')' ;
if_expr = 'if' '(' condition ')' block
          { 'else' 'if' '(' condition ')' block }
          [ 'else' block ] ;
condition = expression | 'let' pattern '=' expression ;
match_expr = 'match' expression '{' match_arm { NEWLINE match_arm } '}' ;
match_arm = pattern [ 'if' '(' expression ')' ] '=>' ( expression | block ) ;
lambda_expr = '(' [ lambda_param { ',' lambda_param } ] ')'
              '=>' ( expression | block ) ;
lambda_param = IDENT [ ':' type_expr ] ;
```

The `condition` production lets `if` and `while` accept either a
plain boolean expression or an `if-let`-style pattern match.
The `lambda_expr` rule is the source of "parens around params
are required."

## Patterns

```ebnf
pattern = pattern_alt { '|' pattern_alt } ;
pattern_alt = '_' | IDENT | 'mut' IDENT | literal
            | type_path [ pattern_fields ] | '(' pattern { ',' pattern } ')'
            | '[' [ pattern { ',' pattern } ] ']'
            | pattern '..' pattern | '..' ;
pattern_fields = '{' pattern_field { ',' pattern_field } [ ',' '..' ] [ ',' ] '}'
               | '(' pattern { ',' pattern } ')' ;
pattern_field = IDENT | IDENT ':' pattern ;
```

See [Patterns](./patterns.md) for examples of every form.

## Statements

```ebnf
statement = let_statement | for_loop | while_loop | loop_statement
          | guard_statement | handling_block | expression NEWLINE | ';' ;
block = '{' { statement } '}' ;
let_statement = 'let' [ 'mut' ] pattern [ ':' type_expr ] '=' expression NEWLINE ;
for_loop = 'for' pattern 'in' expression block ;
while_loop = 'while' '(' condition ')' block ;
loop_statement = 'loop' block ;
guard_statement = 'guard' '(' condition ')' 'else' block ;
handling_block = 'handling' '(' handler_binding
                 { ',' handler_binding } [ ',' ] ')' block ;
handler_binding = type_path 'with' expression ;
```

Notice that `expression NEWLINE` is a statement form — any
expression followed by a newline is a valid statement (its
value is discarded unless it's the last thing in a block).

## Collection Literals

```ebnf
list_literal = '[' [ expression { ',' expression } [ ',' ] ] ']' ;
map_literal = '{' map_entry { ',' map_entry } [ ',' ] '}' ;
map_entry = expression ':' expression ;
set_literal = '#' '{' [ expression { ',' expression } [ ',' ] ] '}' ;
record_construction = type_path '{'
                      [ field_init { field_init } ] '}' ;
field_init = IDENT ':' expression NEWLINE | IDENT NEWLINE
           | '..' expression NEWLINE ;
```

The disambiguation between map literal `{ "k": "v" }`, set
literal `#{...}`, record construction `Type { ... }`, and a
plain block `{ ... }` is handled by the parser via lookahead.
See the [Disambiguation Rules](#disambiguation-rules) below.

## Native / FFI

```ebnf
native_fn_decl = { annotation } [ visibility ] 'native' 'fn' IDENT
                 '(' [ param_list ] ')' [ '->' type_expr ]
                 '{' '`' native_code '`' '}' ;
```

The `native` keyword and backtick-quoted body are reserved
syntax. See §14 of `spec/bock-spec.md` for the FFI design;
the implementation surface is still being built.

## Tests

```ebnf
property_test_decl = 'property' '(' STRING_LITERAL ')' '{'
                     'forall' '(' property_bindings ')' block '}' ;
property_bindings = property_binding { ',' property_binding } [ ',' ] ;
property_binding = IDENT ':' type_expr
                   [ '.' IDENT '(' [ arg_list ] ')' ] ;
```

Property-based tests appear at the top level alongside other
declarations. See §20 of `spec/bock-spec.md` for the testing
surface.

## Disambiguation Rules

A few productions look ambiguous on paper but are resolved by
the parser using fixed rules:

- **Map vs Block.** `{` after a `TYPE_IDENT` → record
  construction. First element matches `expression ':'` → map
  literal. Otherwise → block.
- **Tuple vs Grouping.** `(expr)` → grouping. `(expr, ...)` →
  tuple. A trailing comma forces tuple: `(x,)`.
- **Generics vs Comparison.** Bock uses `[]` for generics, so
  `<` is always comparison. No ambiguity.
- **Bitwise OR vs Pattern Alternative.** `|` is bitwise OR in
  expressions and pattern alternative in patterns. The position
  determines the meaning.
- **Pipe vs Bitwise OR.** `|>` (two characters) and `|` (one
  character) are lexically distinct tokens.
- **Type.ident vs instance.ident.** `TYPE_IDENT.ident(...)` is
  an associated function call. `TYPE_IDENT.ident` without `(`
  is not valid in expression position — type names are not
  values. `value.ident` where `value` is a local is field
  access or method call as usual.

The spec's §21.16 lists the canonical disambiguation rules; the
list above is a working summary.
