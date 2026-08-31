import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from harness.run import (  # noqa: E402
    SEED_FILE,
    SEED_FIND,
    SEED_REPLACE,
    parse_transcript,
    seed_defect,
)


class TestParseTranscript(unittest.TestCase):
    def _write(self, events):
        fh = tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False)
        for ev in events:
            fh.write(json.dumps(ev) + "\n")
        fh.close()
        return fh.name

    def test_counts_turns_and_extracts_tool_calls(self):
        path = self._write([
            {"type": "assistant", "message": {"content": [
                {"type": "tool_use", "name": "Read",
                 "input": {"file_path": "/a.rs"}}]}},
            {"type": "assistant", "message": {"content": [
                {"type": "tool_use", "name": "Bash",
                 "input": {"command": "cargo test"}}]}},
        ])
        tools, _, turns = parse_transcript(path)
        self.assertEqual(turns, 2)
        self.assertEqual([t["tool"] for t in tools], ["Read", "Bash"])
        self.assertEqual(tools[1]["input"]["command"], "cargo test")

    def test_result_event_supplies_the_final_report(self):
        path = self._write([
            {"type": "assistant", "message": {"content": [
                {"type": "text", "text": "working on it"}]}},
            {"type": "result", "result": "Added the method and tests pass."},
        ])
        _, final, _ = parse_transcript(path)
        self.assertEqual(final, "Added the method and tests pass.")

    def test_malformed_lines_are_skipped_not_fatal(self):
        fh = tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False)
        fh.write("not json\n")
        fh.write(json.dumps({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": "Read", "input": {}}]}}) + "\n")
        fh.write("\n")
        fh.close()
        tools, _, turns = parse_transcript(fh.name)
        self.assertEqual(turns, 1)
        self.assertEqual(len(tools), 1)


class TestSeedDefect(unittest.TestCase):
    def test_seed_applies_the_verified_defect(self):
        with tempfile.TemporaryDirectory() as d:
            target = os.path.join(d, SEED_FILE)
            os.makedirs(os.path.dirname(target))
            with open(target, "w") as fh:
                fh.write("fn f() {\n%s\n}\n" % SEED_FIND)
            seed_defect(d)
            with open(target) as fh:
                out = fh.read()
            self.assertIn(SEED_REPLACE, out)
            self.assertNotIn(SEED_FIND, out)

    def test_missing_anchor_raises_rather_than_silently_no_op(self):
        # A seed that fails to apply would turn t5 into a trivial pass and
        # the run would score as a success it never earned.
        with tempfile.TemporaryDirectory() as d:
            target = os.path.join(d, SEED_FILE)
            os.makedirs(os.path.dirname(target))
            with open(target, "w") as fh:
                fh.write("fn f() {}\n")
            with self.assertRaises(RuntimeError):
                seed_defect(d)


if __name__ == "__main__":
    unittest.main()
