# Spec Excerpt: Context System

## @context — Free-Form Intent
```bock
@context("""
  Payment processing module.
  @intent: Process and validate payments.
  @constraint: Must complete within 500ms p99.
  @security: PCI-DSS compliance required.
""")
```
Required in production for modules and public functions.

## @requires — Capabilities
```bock
@requires(Capability.Network, Capability.Storage)
```
Propagates through call graph. Generates platform permissions.

## @performance — Budgets
```bock
@performance(max_latency: 100.ms, max_memory: 50.mb)
```

## @invariant — Verified Constraints
```bock
@invariant(result.len() <= input.len())
```
Static verification attempted, runtime assertion fallback.

## @security — Classification
```bock
@security(level: "confidential", pii: true)
```
Prevents accidental logging, generates audit trails.

## @domain — Tags
```bock
@domain("e-commerce", "checkout")
```

## Context in AIR
Attached as ContextBlock to every node. Inherited through tree
(module → declarations → methods). Capabilities propagate upward.

## Context Inheritance Rules
Module-level annotations propagate to all declarations within.
Declaration-level annotations of the same kind **override**
module-level (not merge), with one exception: `@requires` is
**additive** — declaration capabilities union with module
capabilities.

## Completeness
Production mode enforces: all modules have context, all public
functions have context, all capabilities declared, all effects
declared, security types don't leak.

## Behavioral Modifiers
`@concurrent`, `@managed`, `@deterministic`, `@inline`,
`@cold`, `@hot`, `@deprecated("reason")`.
