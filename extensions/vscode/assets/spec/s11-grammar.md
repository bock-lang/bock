# Spec Excerpt: Formal Grammar (Key Productions)

## Module Structure
```ebnf
source_file = { module_doc } [ module_decl ] { import_decl }
              { top_level_item } ;
top_level_item = { annotation }
    ( fn_decl | record_decl | enum_decl | class_decl
    | trait_decl | platform_trait_decl | impl_block
    | effect_decl | type_alias | const_decl
    | module_handle_decl | property_test_decl ) ;
```

## Function Declaration
```ebnf
fn_decl = [ visibility ] [ 'async' ] 'fn' IDENT
          [ generic_params ] '(' [ param_list ] ')'
          [ '->' type_expr ] [ effect_clause ]
          [ where_clause ] block ;
effect_clause = 'with' type_path { ',' type_path } ;
where_clause = 'where' '(' constraint_list ')' ;
```

## Expression Precedence (see s01-lexical for full table)
```ebnf
expression = assignment_expr ;
assignment_expr = pipe_expr [ assign_op pipe_expr ] ;
pipe_expr = compose_expr { '|>' compose_expr } ;
(... continues through precedence levels ...)
postfix_expr = primary_expr { postfix_op } ;
postfix_op = '(' [args] ')' | '[' expr ']' | '.' IDENT | '?' ;
```

## Disambiguation Rules
- `{` after TYPE_IDENT → record construction
- `{` with first element `expr ':'` → map literal
- `{` otherwise → block
- `(expr)` → grouping; `(expr, ...)` → tuple; `(expr,)` → tuple
- `[]` for generics — `<` is always comparison
- `|` = bitwise OR (expr context) or pattern alt (pattern context)
- `|>` (two chars) vs `|` (one char) — lexically distinct
