// ── Bock Result runtime ──
export type BockResult<T, E> =
  | { readonly _tag: "Ok"; readonly _0: T }
  | { readonly _tag: "Err"; readonly _0: E };
//# sourceMappingURL=_bock_runtime.ts.map
