import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from shim.wirelog import WireLog  # noqa: E402


class TestWireLog(unittest.TestCase):
    def test_records_are_jsonl_in_order(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "wire.jsonl")
            log = WireLog(p)
            log.record("request", {"model": "m"})
            log.record("response", {"ok": True})
            with open(p) as fh:
                lines = fh.read().strip().split("\n")
            self.assertEqual(len(lines), 2)
            self.assertEqual(json.loads(lines[0])["direction"], "request")
            self.assertEqual(json.loads(lines[0])["payload"], {"model": "m"})
            self.assertEqual(json.loads(lines[1])["direction"], "response")

    def test_every_record_has_seq_and_timestamp(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "wire.jsonl")
            log = WireLog(p)
            log.record("request", {})
            log.record("request", {})
            with open(p) as fh:
                recs = [json.loads(line) for line in fh]
            self.assertEqual([r["seq"] for r in recs], [0, 1])
            self.assertLessEqual(recs[0]["ts"], recs[1]["ts"])

    def test_unserialisable_payload_is_logged_not_raised(self):
        # A crash in the logger must never take down a benchmark run.
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "wire.jsonl")
            log = WireLog(p)
            log.record("response", {"bad": object()})
            with open(p) as fh:
                rec = json.loads(fh.readline())
            self.assertIn("unserialisable", rec)


if __name__ == "__main__":
    unittest.main()
