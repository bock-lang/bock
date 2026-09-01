import json
import os
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from harness.run import (  # noqa: E402
    SEED_FILE,
    SEED_FIND,
    SEED_REPLACE,
    assert_model_identity,
    child_env,
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


class TestChildEnv(unittest.TestCase):
    """The benchmarked model runs with permissions bypassed. What it can
    reach is therefore the only thing standing between a disposition probe
    and a real incident."""

    BASE = {
        "PATH": "/usr/bin", "HOME": "/home/x",
        "ANTHROPIC_API_KEY": "real-key",
        "GH_TOKEN": "gh", "GITHUB_TOKEN": "gh2", "AWS_SECRET_ACCESS_KEY": "aws",
        "LLS_BROKER_TOKEN": "broker", "LLS_API_KEY": "dataplane",
        "LLAMA_API_KEY": "llama",
    }

    def _env(self):
        return child_env(8787, "qwopus-coder", base=dict(self.BASE))

    def test_broker_token_is_not_handed_to_the_model_under_test(self):
        # It authenticates the verb set that can stop the very server
        # measuring this run.
        self.assertNotIn("LLS_BROKER_TOKEN", self._env())

    def test_data_plane_keys_are_stripped(self):
        env = self._env()
        self.assertNotIn("LLS_API_KEY", env)
        self.assertNotIn("LLAMA_API_KEY", env)

    def test_cloud_credentials_are_stripped(self):
        env = self._env()
        for k in ("ANTHROPIC_API_KEY", "GH_TOKEN", "GITHUB_TOKEN",
                  "AWS_SECRET_ACCESS_KEY"):
            self.assertNotIn(k, env)

    def test_no_stripped_value_survives_under_any_name(self):
        # A rename would defeat prefix matching; assert on the values.
        leaked = [k for k, v in self._env().items()
                  if v in {"real-key", "gh", "gh2", "aws", "broker",
                           "dataplane", "llama"}]
        self.assertEqual(leaked, [])

    def test_benign_environment_survives(self):
        env = self._env()
        self.assertEqual(env["PATH"], "/usr/bin")
        self.assertEqual(env["HOME"], "/home/x")

    def test_shim_is_wired_as_the_backend(self):
        env = self._env()
        self.assertEqual(env["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787")
        self.assertEqual(env["ANTHROPIC_MODEL"], "qwopus-coder")
        self.assertEqual(env["ANTHROPIC_AUTH_TOKEN"], "local-bench-dummy")


class _ModelsHandler(BaseHTTPRequestHandler):
    served = "qwopus-coder"

    def do_GET(self):
        payload = json.dumps({"data": [{"id": _ModelsHandler.served}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *a):
        pass


class TestModelIdentityGuard(unittest.TestCase):
    """The stale-incumbent hijack is the one failure scoring cannot catch."""

    def setUp(self):
        self.srv = HTTPServer(("127.0.0.1", 0), _ModelsHandler)
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.base = "http://127.0.0.1:%d" % self.srv.server_port

    def tearDown(self):
        self.srv.shutdown()

    def test_passes_when_the_expected_model_is_served(self):
        _ModelsHandler.served = "qwopus-coder"
        self.assertEqual(assert_model_identity(self.base, "qwopus-coder"),
                         ["qwopus-coder"])

    def test_aborts_when_a_different_model_is_served(self):
        # lls starting B while A still listens serves A with ok=true.
        _ModelsHandler.served = "gemma3-12b"
        with self.assertRaises(RuntimeError) as cm:
            assert_model_identity(self.base, "qwopus-coder")
        self.assertIn("gemma3-12b", str(cm.exception))
        self.assertIn("qwopus-coder", str(cm.exception))

    def test_aborts_when_the_upstream_is_unreachable(self):
        with self.assertRaises(RuntimeError):
            assert_model_identity("http://127.0.0.1:1", "qwopus-coder")


if __name__ == "__main__":
    unittest.main()
