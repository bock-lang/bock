import json
import time
import os
import sys
import tempfile
import threading
import unittest
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from shim.server import (  # noqa: E402
    make_server,
    make_upstream_gate,
    call_upstream,
)


class MockUpstream(BaseHTTPRequestHandler):
    """Stands in for llama-server's OpenAI-compatible endpoint."""

    captured = None
    captured_headers = None

    def do_POST(self):
        MockUpstream.captured_headers = dict(self.headers)
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        MockUpstream.captured = body
        payload = json.dumps({
            "choices": [{"message": {"role": "assistant", "content": "hi"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 2},
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *a):
        pass


class ToolCallUpstream(MockUpstream):
    def do_POST(self):
        json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        payload = json.dumps({"choices": [{"message": {
            "role": "assistant", "content": None,
            "tool_calls": [{"id": "call_1", "type": "function", "function": {
                "name": "Edit",
                "arguments": json.dumps({"file_path": "/a.rs",
                                         "old_string": "fn a() {}",
                                         "new_string": "fn b() {}"})}}]},
            "finish_reason": "tool_calls"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def _serve(handler_cls):
    srv = HTTPServer(("127.0.0.1", 0), handler_cls)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def _post(url, body):
    req = urllib.request.Request(
        url, data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())


class TestServer(unittest.TestCase):
    def setUp(self):
        self.up = _serve(MockUpstream)
        self.dir = tempfile.mkdtemp()
        self.wire = os.path.join(self.dir, "wire.jsonl")
        self.shim = make_server(
            port=0,
            upstream="http://127.0.0.1:%d" % self.up.server_port,
            alias="test-alias", wire_log_path=self.wire,
            background_upstream=None)
        threading.Thread(target=self.shim.serve_forever, daemon=True).start()
        self.base = "http://127.0.0.1:%d" % self.shim.server_port

    def tearDown(self):
        self.shim.shutdown()
        self.up.shutdown()

    def test_messages_returns_anthropic_shaped_response(self):
        out = _post(self.base + "/v1/messages",
                    {"model": "x", "max_tokens": 64,
                     "messages": [{"role": "user", "content": "hi"}]})
        self.assertEqual(out["type"], "message")
        self.assertEqual(out["role"], "assistant")
        self.assertEqual(out["content"][0]["type"], "text")
        self.assertEqual(out["content"][0]["text"], "hi")
        self.assertEqual(out["stop_reason"], "end_turn")
        self.assertEqual(out["usage"]["input_tokens"], 10)

    def test_model_id_is_rewritten_to_the_alias(self):
        _post(self.base + "/v1/messages",
              {"model": "claude-opus-5", "max_tokens": 8,
               "messages": [{"role": "user", "content": "hi"}]})
        self.assertEqual(MockUpstream.captured["model"], "test-alias")

    def test_count_tokens_endpoint_exists(self):
        out = _post(self.base + "/v1/messages/count_tokens",
                    {"model": "x", "messages":
                     [{"role": "user", "content": "hello there"}]})
        self.assertIn("input_tokens", out)
        self.assertGreater(out["input_tokens"], 0)

    def test_models_endpoint_lists_the_alias(self):
        with urllib.request.urlopen(self.base + "/v1/models", timeout=10) as r:
            out = json.loads(r.read())
        self.assertEqual(out["data"][0]["id"], "test-alias")

    def test_request_and_response_are_both_wire_logged(self):
        _post(self.base + "/v1/messages",
              {"model": "x", "max_tokens": 8,
               "messages": [{"role": "user", "content": "hi"}]})
        with open(self.wire) as fh:
            recs = [json.loads(line) for line in fh]
        dirs = [r["direction"] for r in recs]
        self.assertIn("request", dirs)
        self.assertIn("response", dirs)


class TestToolCallRoundTrip(unittest.TestCase):
    def setUp(self):
        self.up = _serve(ToolCallUpstream)
        self.dir = tempfile.mkdtemp()
        self.shim = make_server(
            port=0, upstream="http://127.0.0.1:%d" % self.up.server_port,
            alias="a", wire_log_path=os.path.join(self.dir, "w.jsonl"),
            background_upstream=None)
        threading.Thread(target=self.shim.serve_forever, daemon=True).start()
        self.base = "http://127.0.0.1:%d" % self.shim.server_port

    def tearDown(self):
        self.shim.shutdown()
        self.up.shutdown()

    def test_edit_tool_call_survives_with_old_string_intact(self):
        out = _post(self.base + "/v1/messages",
                    {"model": "x", "max_tokens": 8,
                     "messages": [{"role": "user", "content": "edit it"}]})
        self.assertEqual(out["stop_reason"], "tool_use")
        block = [b for b in out["content"] if b["type"] == "tool_use"][0]
        self.assertEqual(block["name"], "Edit")
        self.assertEqual(block["id"], "call_1")
        self.assertEqual(block["input"]["old_string"], "fn a() {}")
        self.assertEqual(block["input"]["new_string"], "fn b() {}")


class TestUpstreamAuth(unittest.TestCase):
    """llama-server --api-key expects a bearer token.

    Without this the benchmark 401s mid-campaign the moment the data plane
    is token-guarded, and it looks like a model failure.
    """

    def _run(self, key):
        up = _serve(MockUpstream)
        d = tempfile.mkdtemp()
        shim = make_server(
            port=0, upstream="http://127.0.0.1:%d" % up.server_port,
            alias="a", wire_log_path=os.path.join(d, "w.jsonl"),
            background_upstream=None, upstream_api_key=key)
        threading.Thread(target=shim.serve_forever, daemon=True).start()
        try:
            _post("http://127.0.0.1:%d/v1/messages" % shim.server_port,
                  {"model": "x", "max_tokens": 8,
                   "messages": [{"role": "user", "content": "hi"}]})
            return {k.lower(): v
                    for k, v in (MockUpstream.captured_headers or {}).items()}
        finally:
            shim.shutdown()
            up.shutdown()

    def test_bearer_token_is_forwarded_upstream(self):
        self.assertEqual(self._run("s3cret").get("authorization"),
                         "Bearer s3cret")

    def test_no_header_when_no_key_configured(self):
        self.assertNotIn("authorization", self._run(None))


if __name__ == "__main__":
    unittest.main()


class _FakeResponse:
    def __init__(self, payload):
        self._payload = payload

    def read(self):
        return self._payload

    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


class TestUpstreamIsSerialized(unittest.TestCase):
    """One model, one slot: overlapping requests must not contend.

    Claude Code fires a session-title request ~0.07s after the agent request.
    llama-server runs with --parallel 1, so the two contend for a single slot,
    and the agent response came back as prose (`<tool>Read</tool>`) with no
    tool call. Replayed alone the identical request produced a correct tool
    call 5 times out of 5, so the contention is the defect, not the model.

    Raising --parallel is the wrong fix: it splits the KV cache across slots
    and would silently halve the context we deliberately equalised.
    """

    def test_concurrent_requests_do_not_overlap_upstream(self):
        import threading
        overlap = []
        active = []
        lock = threading.Lock()

        def fake_urlopen(req, timeout=None):
            with lock:
                active.append(1)
                if len(active) > 1:
                    overlap.append(True)
            time.sleep(0.05)
            with lock:
                active.pop()
            return _FakeResponse(json.dumps({
                "choices": [{"finish_reason": "stop", "index": 0,
                             "message": {"role": "assistant", "content": "ok"}}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1},
            }).encode())

        gate = make_upstream_gate()
        threads = [threading.Thread(target=lambda: call_upstream(
            gate, fake_urlopen, "http://x", {}, {})) for _ in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(overlap, [], "upstream calls overlapped")
