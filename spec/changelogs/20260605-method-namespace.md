# Single method namespace per type (DQ27)

**Date:** 2026-06-05
**Affects:** §6.4 (Classes), §6.5 (Traits), §6.7 (Impl Blocks)
**Type:** clarification, addition

## Change

A type (`record` or `class`) has exactly **one method namespace**, keyed by
method name. Every method that applies to the type contributes to that single
namespace, whether declared in an inherent `impl T { }` block, in a `class T { }`
body, or in a trait `impl Trait for T { }` block.

Three normative rules are added to §6.7 (with cross-references from §6.4 and
§6.5):

1. **Trait requirements are satisfied by a name + signature match anywhere in
   the namespace.** An `impl Trait for T { }` block may be empty when an
   inherent or class-body method of matching name and signature already provides
   the required method. There is no distinction between an "inherent" and a
   "trait" method of the same name — there is one method.

2. **Defining the same method name more than once for one type is a coherence
   error (`E4012`)**, regardless of which blocks the definitions appear in. This
   holds whether the duplicate signatures match (redundant) or differ.

3. **A type cannot satisfy two traits that require the same method name with
   incompatible signatures.** On the v1 targets a type has one method slot per
   name, so this is genuinely unsatisfiable and is reported as `E4012` rather
   than resolved by name-mangling.

The parameterized conversion traits `From[T]` / `Into[T]` and generic blanket
impls are exempt: each `From[…]`/`Into[…]` instantiation is selected by its
trait argument (not by a bare `.from()`/`.into()` call), so a method name shared
across instantiations is not a namespace collision.

The implementation adds the `E4012` duplicate-method coherence check to the type
checker's `ImplTable` construction (`bock-types`). The checker change is the
primary fix; because the duplicate form is now rejected before execution, a
program that passes `bock check` can never reach an infinitely-recursive
self-forwarding method, so the previously-observed cross-target runtime
divergence (a `self.render()` forwarder that stack-overflowed on js/ts and the
reference interpreter) is unreachable for well-formed programs.

## Rationale

Resolves design question DQ27. Sections §6.4/§6.5/§6.7 were previously silent on
the inherent-vs-trait same-name case. The single-method-namespace model is the
only model that maps cleanly to **every** v1 target: js, ts, python, and Go
structs all have a single method slot per name, so two same-named methods cannot
be represented idiomatically anywhere except Rust. Auto-satisfying a trait
requirement from an inherent/class-body method (rule 1) subsumes the useful part
of merely forbidding same-name inherent+trait methods, while the duplicate-
definition error (rule 2) keeps the namespace unambiguous.

The `examples/target-optimized/react-components` example previously declared both
an inherent `render` and a redundant `impl Component for Button { fn render =
self.render() }`. Under the single-method-namespace rule those collapse to one
`render`, which on the overload-less targets had aliased into the self-recursive
forwarder. The example now defines `render` exactly once (in the `Component`
impl) and builds + runs on all five targets without recursion.

## Migration

A type that declared the same method name in more than one block (e.g. an
inherent method plus a same-named trait-impl method) now fails `bock check` with
`E4012`. Define the method **once**:

- put the method body in the inherent `impl`/class body and leave
  `impl Trait for T { }` empty (the inherent method satisfies the trait); or
- put the method body inside `impl Trait for T { }` and do not also declare it
  inherently.

Either form satisfies the trait and answers direct `value.method()` calls. No
change is required for types whose method names were already distinct across
blocks, nor for `From[T]`/`Into[T]` conversion impls.
