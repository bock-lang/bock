"""Anthropic Messages API shim over llama-server's OpenAI endpoint.

Claude Code speaks ONLY the Anthropic Messages API. Verified against CLI
2.1.251: `chat/completions` appears 0 times in the binary; `v1/messages`
57 times. Pointing ANTHROPIC_BASE_URL straight at llama-server 404s on
every request. This process is the translation layer that makes it work.
"""

import argparse
import json
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from .translate import anthropic_to_openai, count_tokens_estimate
from .wirelog import WireLog

_STOP_REASON = {"stop": "end_turn", "length": "max_tokens",
                "tool_calls": "tool_use"}


def _openai_to_anthropic(resp, model):
    """Translate a non-streaming OpenAI response back to Anthropic shape."""
    choice = (resp.get("choices") or [{}])[0]
    msg = choice.get("message", {}) or {}
    content = []
    if msg.get("content"):
        content.append({"type": "text", "text": msg["content"]})
    for tc in msg.get("tool_calls") or []:
        fn = tc.get("function", {})
        try:
            args = json.loads(fn.get("arguments") or "{}")
        except ValueError:
            args = {}
        content.append({"type": "tool_use", "id": tc.get("id", ""),
                        "name": fn.get("name", ""), "input": args})
    usage = resp.get("usage", {}) or {}
    return {
        "id": resp.get("id", "msg_shim"),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": _STOP_REASON.get(choice.get("finish_reason"), "end_turn"),
        "stop_sequence": None,
        "usage": {"input_tokens": usage.get("prompt_tokens", 0),
                  "output_tokens": usage.get("completion_tokens", 0)},
    }


def make_server(port, upstream, alias, wire_log_path,
                background_upstream=None, background_alias=None):
    wire = WireLog(wire_log_path)

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass

        def _send_json(self, obj, code=200):
            payload = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_GET(self):
            if self.path.startswith("/v1/models"):
                self._send_json({"object": "list", "data": [
                    {"id": alias, "object": "model", "type": "model"}]})
            else:
                self._send_json({"error": "not found"}, 404)

        def do_POST(self):
            length = int(self.headers.get("Content-Length") or 0)
            raw = self.rfile.read(length)
            try:
                body = json.loads(raw or b"{}")
            except ValueError:
                self._send_json({"type": "error", "error":
                                 {"type": "invalid_request_error"}}, 400)
                return

            if self.path.startswith("/v1/messages/count_tokens"):
                self._send_json({"input_tokens": count_tokens_estimate(body)})
                return
            if not self.path.startswith("/v1/messages"):
                self._send_json({"error": "not found"}, 404)
                return

            # Background/haiku-class calls must not land on the model under
            # test: they would corrupt every wall-clock measurement.
            requested = body.get("model") or ""
            is_background = "haiku" in requested.lower()
            target = upstream
            target_alias = alias
            if is_background and background_upstream:
                target, target_alias = background_upstream, background_alias

            oai = anthropic_to_openai(body)
            oai["model"] = target_alias
            # Streaming is translated back non-streaming; Claude Code accepts
            # a complete message response.
            oai.pop("stream", None)

            wire.record("request", oai,
                        {"requested_model": requested,
                         "background": is_background, "upstream": target})
            try:
                req = urllib.request.Request(
                    target.rstrip("/") + "/v1/chat/completions",
                    data=json.dumps(oai).encode(),
                    headers={"Content-Type": "application/json"},
                    method="POST")
                with urllib.request.urlopen(req, timeout=600) as r:
                    upstream_resp = json.loads(r.read())
            except Exception as exc:  # upstream failure is a run result
                wire.record("error", {"error": repr(exc)})
                self._send_json({"type": "error", "error":
                                 {"type": "api_error",
                                  "message": repr(exc)}}, 502)
                return

            wire.record("response", upstream_resp)
            self._send_json(_openai_to_anthropic(upstream_resp, requested))

    return ThreadingHTTPServer(("127.0.0.1", port), Handler)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8787)
    ap.add_argument("--upstream", required=True,
                    help="llama-server base URL, e.g. http://172.20.0.1:8160")
    ap.add_argument("--alias", required=True,
                    help="llama-server --alias for the model under test")
    ap.add_argument("--wire-log", required=True)
    ap.add_argument("--background-upstream", default=None)
    ap.add_argument("--background-alias", default=None)
    args = ap.parse_args()
    srv = make_server(args.port, args.upstream, args.alias, args.wire_log,
                      args.background_upstream, args.background_alias)
    print("shim listening on http://127.0.0.1:%d -> %s (%s)"
          % (srv.server_port, args.upstream, args.alias), flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
