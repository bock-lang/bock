// ── Bock concurrency runtime ──
export const __bockChannelNew = () => {
  const queue = [];
  const waiters = [];
  const ch = {
    send(v) {
      if (waiters.length > 0) { waiters.shift()(v); } else { queue.push(v); }
    },
    recv() {
      return new Promise((resolve) => {
        if (queue.length > 0) { resolve(queue.shift()); }
        else { waiters.push(resolve); }
      });
    },
    close() {}
  };
  return [ch, ch];
};
export const __bockSpawn = (x) => x;
//# sourceMappingURL=_bock_runtime.js.map
