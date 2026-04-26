# Spec Excerpt: Bock Intermediate Representation (AIR)

## Four Layers
- **S-AIR (Structural):** Syntax tree, resolved names/scopes
- **T-AIR (Typed):** Types resolved, ownership, effects, capabilities
- **C-AIR (Contextual):** Context annotations attached/validated
- **TR-AIR (Target-Ready):** Capability gaps, platform abstractions

All layers deterministic. AI enters only after AIR production.

## Node Structure
```
AIRNode {
  id: NodeId
  kind: NodeKind              // children typed per variant
  span: SourceSpan
  type: TypeInfo              // Layer 1+
  ownership: OwnershipInfo    // Layer 1+
  effects: Set[Effect]        // Layer 1+
  capabilities: Set[Cap]      // Layer 1+
  context: ContextBlock       // Layer 2+
  target: TargetInfo          // Layer 3+
  metadata: Map[String, Value]
}
```
Children are structurally typed within each NodeKind variant
(e.g., FnDecl has params, body, return_type fields), not a
flat `List[AIRNode]`.

## Node Kind Categories
- Module: ModuleDecl, ImportDecl
- Declarations: FnDecl, RecordDecl, EnumDecl, ClassDecl,
  TraitDecl, ImplBlock, EffectDecl, TypeAlias, ConstDecl
- Expressions: Literal, BinaryOp, UnaryOp, Call, MethodCall,
  FieldAccess, Index, Lambda, Pipe, Compose, Await, Range
- Control: If, Guard, Match, For, While, Loop, Break, Continue,
  Return, Block
- Ownership: LetBinding, Move, Borrow, MutableBorrow
- Error: Propagate, ResultConstruct
- Effects: EffectOp, HandlingBlock, EffectRef
- Patterns: WildcardPat, BindPat, LiteralPat, ConstructorPat,
  TuplePat, ListPat, OrPat, GuardPat, RestPat

## Serialization
- AIR-T (text): human/AI-readable, for transpilation input
- AIR-B (binary): compact, content-addressed, for caches
