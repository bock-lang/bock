# Modules

> Stub. Full coverage in §12 (Module System) of
> [`spec/bock-spec.md`](../../../spec/bock-spec.md).

Each Bock file that participates in cross-file imports declares its
module name on the first non-comment line:

```bock
module geometry

public fn area(r: Float) -> Float { 3.14159 * r * r }
```

## Visibility

- `public` — exported from the module.
- (default) — private to the file.

## Imports

```bock
use std.collections.{List, Map}
use geometry as geo
```

## Project Structure

A Bock project is rooted at a `bock.project` (TOML) file. Modules
declared anywhere under that root resolve through the module
registry. See the [CLI reference](../reference/cli.md) and the
spec section for path resolution rules.
