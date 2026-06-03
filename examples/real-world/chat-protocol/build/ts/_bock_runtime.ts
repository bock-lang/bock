// ── Bock Optional runtime ──
export type BockOption<T> =
  | { readonly _tag: "Some"; readonly _0: T }
  | { readonly _tag: "None" };
// ── Bock Result runtime ──
export type BockResult<T, E> =
  | { readonly _tag: "Ok"; readonly _0: T }
  | { readonly _tag: "Err"; readonly _0: E };
// ── Bock concurrency runtime ──
export type __BockChannel<T> = {
  send(v: T): void;
  recv(): Promise<T>;
  close(): void;
};
export const __bockChannelNew = <T>(): [__BockChannel<T>, __BockChannel<T>] => {
  const queue: T[] = [];
  const waiters: Array<(v: T) => void> = [];
  const ch: __BockChannel<T> = {
    send(v: T) {
      if (waiters.length > 0) { waiters.shift()!(v); } else { queue.push(v); }
    },
    recv(): Promise<T> {
      return new Promise<T>((resolve) => {
        if (queue.length > 0) { resolve(queue.shift()!); }
        else { waiters.push(resolve); }
      });
    },
    close() {}
  };
  return [ch, ch];
};
export const __bockSpawn = <T>(x: Promise<T>): Promise<T> => x;
//# sourceMappingURL=_bock_runtime.ts.map
