# Modules

A Bock project is organised into files. Each file that should
participate in cross-file imports declares its module name on
the first non-comment line. Imports use a dotted-path syntax
that mirrors the filesystem structure, and the module registry
resolves every name against `bock.project` at the root of the
project.

## File-Based Modules

Every importable `.bock` file begins with a `module` declaration
whose path matches the file's location under `src/`:

```
src/main.bock              → module main
src/math.bock              → module math
src/utils/strings.bock     → module utils.strings
src/app/auth/session.bock  → module app.auth.session
```

The module path must match the path on disk. A mismatch is a
compile-time error caught by `bock check`.

```bock
module utils.strings

public fn shout(s: String) -> String {
  "${s}!"
}
```

<!-- verify: bock-check -->

A file without a `module` declaration is a *script* — it can be
compiled and run on its own with `bock check path/to/file.bock`
or `bock run`, but it cannot be imported by other files.

## Imports: `use`

Cross-file imports use the `use` statement, written at the top
of the file (after `module` and before any other declarations):

```bock
module main

use math.{double_it, square}

fn main() {
  println("${double_it(3)} ${square(4)}")
}
```

This file imports `double_it` and `square` from the `math`
module. The braced import list selects the specific symbols
that come into scope.

The companion `math` module:

```bock
module math

public fn double_it(n: Int) -> Int { n * 2 }
public fn square(n: Int) -> Int { n * n }
fn private_helper() -> Int { 0 }
```

In a project, these two files compile together as part of the
same crate. Symbols declared `public` (like `double_it` and
`square`) are visible to imports; symbols without `public` —
or, depending on strictness, declared explicitly without any
visibility — remain private to the declaring file.

### Wildcard Imports

`use module.*` imports every public symbol from the module:

```bock
module main

use math.*

fn main() {
  println("${double_it(3)} ${square(4)}")
}
```

Wildcard imports are discouraged in larger codebases because
they obscure which file a name came from. They are convenient
for tightly-coupled modules and for tests.

### Nested Module Paths

A dotted path crosses directories:

```bock
module main

use utils.strings.{shout}

fn main() {
  println(shout("hello"))
}
```

The path `utils.strings` resolves to `src/utils/strings.bock`
(or to a `src/utils/strings/` directory with a `mod.bock`
inside, by the same convention as Rust). The braced import list
selects names to pull in.

## Visibility

Three visibility modifiers govern who can see a declaration:

| Modifier | Scope |
|----------|-------|
| (default) | Visible only in the declaring file. |
| `internal` | Visible within the module tree. |
| `public` | Visible everywhere a `use` reaches it. |

```bock
module example.calc

public fn add(a: Int, b: Int) -> Int { a + b }
internal fn normalize(x: Int) -> Int { if (x < 0) { 0 } else { x } }
fn private_only(x: Int) -> Int { x * 2 }
```

A `public fn` is part of the module's public API. `internal`
sits between `public` and private — visible to other modules
inside the same project's module tree but not to external
packages depending on the project as a library.

Visibility applies to every top-level declaration: `fn`,
`record`, `enum`, `class`, `trait`, `effect`, `const`, `type`.
The same rule applies to enum variants and to record fields —
they inherit visibility from the enclosing declaration unless
narrowed explicitly.

The default visibility level varies by project strictness. In
`sketch`, undecorated declarations are effectively public; in
`production`, the default is private and visibility must be
explicit. See §10.7 of `spec/bock-spec.md` for the full table.

## Re-exports

A module can re-export symbols from other modules to define a
flat public API surface. Place the re-exports in a `mod.bock`
file inside the directory:

```bock
module app.models

public use app.models.user.{User}
public use app.models.session.{Session}
```

External code now imports `User` and `Session` directly from
`app.models` without knowing they live in submodules. This
lets you reorganise internals without breaking downstream
imports.

## Module Registry

The compiler discovers modules by walking the directory tree
rooted at `bock.project`. Every `.bock` file with a `module`
declaration is added to the registry; the dotted path becomes
the canonical name.

```
my-project/
├── bock.project
└── src/
    ├── main.bock            → module main
    ├── math.bock            → module math
    └── utils/
        ├── mod.bock         → module utils      (re-export root)
        └── strings.bock     → module utils.strings
```

`bock check` and `bock build` walk the registry, type-check
every module, and resolve cross-file `use` statements against
it. There is no separate manifest of modules — the directory
structure is the manifest.

## What Currently Works

This page describes the working surface. A few module-system
forms from the spec are not yet implemented:

- **`use module as alias`** — the spec describes an alias form
  (e.g., `use geometry as geo`). The current parser does not
  yet accept it. Use the braced form (`use geometry.{...}`)
  for now.
- **`use module.name`** — single dotted import without braces
  (e.g., `use math.double_it`) is not yet resolved by the name
  resolver. Use `use math.{double_it}` instead.

Both forms are reserved syntax; their semantics will land in
later compiler releases. The braced and wildcard forms cover
every import scenario in the current implementation.

For a comprehensive reference on cross-file resolution rules,
re-exports, and visibility composition, see §12 of
`spec/bock-spec.md`.
