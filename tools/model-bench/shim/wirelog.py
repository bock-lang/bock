"""Verbatim request/response logging.

This is what distinguishes "the model emitted a malformed tool call" from
"the shim dropped old_string". Without it, a transport bug is recorded as
model quality, which is the exact confound the benchmark must rule out.
"""

import json
import threading
import time


class WireLog:
    def __init__(self, path):
        self._path = path
        self._lock = threading.Lock()
        self._seq = 0

    def record(self, direction, payload, meta=None):
        with self._lock:
            rec = {"seq": self._seq, "ts": time.time(),
                   "direction": direction, "meta": meta or {}}
            self._seq += 1
            try:
                json.dumps(payload)
                rec["payload"] = payload
            except (TypeError, ValueError):
                rec["unserialisable"] = repr(payload)[:4000]
            with open(self._path, "a") as fh:
                fh.write(json.dumps(rec) + "\n")
