# Spec Excerpt: Module System

## File-Based Modules
```
src/app/auth.bock → module app.auth
```
Each file declares module path matching filesystem path.

## Module Declaration
```bock
module app.auth
```
Must be first non-comment item.

## Imports
```bock
use core.collections.{List, Map}   // named
use app.models.User                // single
use app.services.*                 // wildcard (discouraged)
```

## Import Grammar
```ebnf
import_decl = 'use' module_path [ import_list ] ;
import_list = '.' '{' IDENT { ',' IDENT } '}'
            | '.' IDENT | '.' '*' ;
```

## Visibility
- (default): private to file
- `internal`: visible within module tree
- `public`: visible everywhere

Default visibility varies by strictness:
- sketch: more permissive
- production: private by default

## Re-exports
```bock
// In mod.bock
public use app.models.user.User
```

## Module Index
`mod.bock` serves as the module index file (like mod.rs or
__init__.py). Re-exports define the module's public API.
