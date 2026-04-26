# New Crate

Scaffold a new workspace crate under `compiler/crates/`.

## Arguments

`$ARGUMENTS` — the crate name without the `bock-` prefix.

Example: `/project:new-crate effects` creates `compiler/crates/bock-effects/`.

## Steps

1. **Confirm the name follows convention.** All compiler crates are
   prefixed `bock-`. The argument should be the suffix only.

2. **Create the directory structure:**
   ```
   compiler/crates/bock-<name>/
     Cargo.toml
     src/lib.rs
     tests/smoke.rs
   ```

3. **`Cargo.toml` template:**
   ```toml
   [package]
   name = "bock-<name>"
   version.workspace = true
   edition.workspace = true
   license.workspace = true
   repository.workspace = true
   authors.workspace = true
   rust-version.workspace = true

   [lints]
   workspace = true

   [dependencies]
   bock-errors = { path = "../bock-errors" }

   [dev-dependencies]
   ```

4. **`src/lib.rs` template:**
   ```rust
   //! bock-<name> — short description of this crate's purpose.

   // Crate body goes here.
   ```

5. **`tests/smoke.rs` template:**
   ```rust
   #[test]
   fn crate_compiles() {
       // Sanity test — replace with real coverage as the crate grows.
   }
   ```

6. **Verify workspace pickup.** The root `Cargo.toml` uses
   `compiler/crates/*` as a glob, so the new crate is included
   automatically. Confirm:
   ```
   cargo check -p bock-<name>
   cargo test -p bock-<name>
   ```

7. **Update `ARCHITECTURE.md`** if the new crate changes the
   dependency order diagram. Otherwise leave it alone.

## Done When

- `cargo check -p bock-<name>` succeeds
- `cargo test -p bock-<name>` runs the smoke test
- A commit on a feature branch contains the new crate
