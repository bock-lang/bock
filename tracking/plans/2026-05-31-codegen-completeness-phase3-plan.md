# Phase 3 — Codegen-Completeness Milestone: Go collection typing + Map/Set + breadth

Plan agent (read-only) @ main 7806e8d (now 6cc5b9e post-#143). RE-GREP anchors (lines shift per PR). Last
codegen-completeness phase before stdlib R1 resumes (P4 = polish). Reuses #137 `recv_kind` + #138/#129
`desugared_*_method` recognizers.

## Ground truth
- Map/Set ALREADY get recv_kind tags (checker.rs recv_kind_tag → "Map"/"Set"/"List"); the checker fully resolves
  Map/Set/String method return types (Map.get→Optional[V], keys/values→List, len→Int, contains_key/is_empty→Bool;
  Set add/…→Set, contains/…→Bool). **Map/Set dispatch is a pure CODEGEN problem.** NAMING: checker uses
  `contains_key` for Map (audit/spec say `m.contains`) → Q-map-contains-name (recognizer keys on checker's spelling
  or add a `contains` alias).
- `desugared_list_method` (generator.rs:840) matches READ_ONLY_LIST_METHODS WITHOUT consulting recv_kind → on a
  Map/Set receiver, get/contains/len wrongly route through List; set/add/keys/values fall to desugared_self_call →
  `m.set(m,k,v)` (undefined). Root cause of item 2.
- Go collection element typing hardcoded `interface{}` at 3 sites: literals (go.rs:3593 List/3604 Map/3617 Set),
  declared types (map_type_name go.rs:4839 + is_mapped_runtime suppresses the `[T]` arg in type_to_go:4786/
  ast_type_to_go:4871), and `for x in list` (go.rs:3018 — nothing inserted into var_go_type). js/ts/py/rust Lists
  natively typed → **Go-only for Lists**. Levers: var_list_elem/var_optional_elem/var_go_type + infer_go_expr_type(1167).
- range() has NO runtime on js/ts (bare `range(...)` call, zero defs) / Go emits `/* range */ nil`; py/rust native.
- Go record-spread no-op (go.rs:3560 drops `..p`). Go Self-in-plain-impl: go_self_subst only set for trait impls
  (use_value_receiver, go.rs:2304/2706) → plain impl `Self`→`/* Self */` (#141 OPEN). Go nested-match-payload arith:
  var_go_type assertion doesn't recurse into nested Constructor/Record payloads (#142 FOUND).

## Sequence: P3-α → P3-β (both go.rs → sequential)
### P3-α — Go collection typing + Go-locals (go.rs only): items 1, 5, 6-go
- (1a declared types) explicit List/Map/Set arms in type_to_go/ast_type_to_go BEFORE map_type_name, emitting
  `[]<elem>`/`map[<k>]<v>`/`map[<elem>]struct{}` recursively over type args (lift is_mapped_runtime erasure for these 3;
  keep it for Optional/Result). (1b literals 3593/3604/3617) infer homogeneous elem type via infer_go_expr_type;
  concrete iff ALL elems infer concretely, else interface{} (never wrong); extend infer_go_expr_type for collection
  literals. (1c for-iteration 3018) insert the loop var into var_go_type for the body scope when the elem Go type is
  recoverable (save/restore like enter_param_optional_scope:1266).
- (5 record-spread 3560) emit IIFE `func() T { __s := <base>; __s.Field=val; …; return __s }()`.
- (6-go-self #141) emit_method go.rs:2706 — set go_self_subst = Some(receiver_type) for ALL impl methods, not just
  use_value_receiver. One-condition fix.
- (6-go-nested #142) recurse var_go_type payload-assertion into nested Constructor/Record payloads. If it needs
  match-lowering surgery (not just scope-map recursion) → split to P4 + OPEN.
- Risk: over-typing (commit concrete only when all infer concretely); scope save/restore.
- Fixtures (all 5; Go-focused, trivially pass elsewhere): go_typed_list_iter, go_typed_map_decl, record_spread,
  self_in_plain_impl, nested_match_arith.

### P3-β — Map/Set dispatch + literals + range() (generator.rs + all 5): items 2, 3-rest, 4
- (2) MAP_METHODS/SET_METHODS const slices + desugared_map_method/desugared_set_method in generator.rs (mirror
  desugared_optional_method:1011), gated on container_recv_kind=="Map"/"Set" (extend container_recv_kind:983). Each
  backend wires try_emit_map/set_method into its Call arm BEFORE desugared_list_method. js/ts native Map/Set; py
  dict/set; rust HashMap/HashSet; go `m[k]`/`v,ok:=`/`len`/`m[k]=v` (builds on α's typed maps). Map.get→Optional[V]
  reuses the Optional rep. recv_kind disambiguates the List/Map/Set overlap (filter/map/len/contains/to_list).
- (3 literals) Set literal works on most (verify); js/ts Map literal failures are LARGELY item-2 artifacts (m.get/set
  misdispatch) — verify in T1, fix only residual. Go Map/Set literal typing → done in P3-α.
- (4 range) recommend (A) runtime helper: RANGE_RUNTIME_{JS,TS} + module_uses_range scan + range_runtime_emitted
  flag (mirror Optional-runtime injection ts.rs:55/286/1139); Go `__bockRange(lo,hi,inclusive)[]int64`. (B alt:
  lower For{Range} to native C-loop — only for-position.) Match py/rust inclusive/exclusive exactly.
- T1 (front-load): map_get_set red on ≥4 before → green ×5 (confirms recv_kind Map/Set tags reach codegen, per #138's
  crux lesson); range_for red js/ts/go → green ×5.

## Gating: core.collections(R3)=item 2+1; core.iter(R1)+core.string(R2) List-heavy on Go = item 1 (R1 critical
path for Go); range() gates iter combinators / test helpers (js/ts/go).
## Design Qs: Q-map-contains-name (contains_key vs contains); Q-range-runtime-vs-native (rec A); Q-mutating-collections
(set/add value-vs-mut-self, DQ18 family — v1 in-place + return receiver); TS Self-in-plain-impl (TS2526 → recommend P4);
Go empty-literal → interface{} fallback (acceptable).
## Gate (every session): build fresh in-worktree (`cargo build -p bock`; NEVER `cd /opt/claude-projects/bock`); fmt;
clippy --workspace --all-targets -D warnings; test --workspace; cargo doc -D warnings; REQUIRE=all run-conformance.
