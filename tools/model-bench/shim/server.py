"""Anthropic Messages API shim over llama-server's OpenAI endpoint.

Claude Code speaks ONLY the Anthropic Messages API. Verified against CLI
2.1.251: `chat/completions` appears 0 times in the binary; `v1/messages`
57 times. Pointing ANTHROPIC_BASE_URL straight at llama-server 404s on
every request. This process is the translation layer that makes it work.
"""

import argparse
import json
import threading
import os
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from .translate import (anthropic_to_openai, count_tokens_estimate,
                        is_session_title_request, session_title_response)
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



def make_upstream_gate():
    """Serialises upstream calls: one model, one slot.

    llama-server runs with --parallel 1 - deliberately, since splitting the
    KV cache across slots would halve the context we equalised at 65536.
    Claude Code, meanwhile, fires a session-title request ~0.07s after the
    agent request. Both hit the shim, which is a ThreadingHTTPServer, so both
    reach llama-server at once and contend for the single slot. Observed
    result: the agent response came back as prose (`<tool>Read</tool>`) with
    no tool call, while the identical request replayed alone produced a
    correct tool call 5/5. Serialising here fixes it without touching the
    model's context budget.
    """
    return threading.Lock()


def call_upstream(gate, urlopen, url, headers, payload, timeout=600):
    """POST to the upstream chat endpoint, one caller at a time."""
    req = urllib.request.Request(
        url.rstrip("/") + "/v1/chat/completions",
        data=json.dumps(payload).encode(),
        headers=headers,
        method="POST")
    with gate:
        with urlopen(req, timeout=timeout) as r:
            return json.loads(r.read())


def make_server(port, upstream, alias, wire_log_path,
                background_upstream=None, background_alias=None,
                upstream_api_key=None, max_output_tokens=None,
                suppress_session_titles=True, session_title="Benchmark run"):
    wire = WireLog(wire_log_path)
    gate = make_upstream_gate()

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

            if suppress_session_titles and is_session_title_request(body):
                # Answered here, never sent upstream: it is not part of the
                # agent loop, and its prompt would otherwise sit in the
                # single slot's cache and contaminate the next agent turn.
                canned = session_title_response(session_title)
                wire.record("short_circuit", canned,
                            {"reason": "session-title request answered by "
                                       "the shim; not sent to the model"})
                self._send_json(_openai_to_anthropic(canned, requested))
                return

            oai = anthropic_to_openai(body, max_output_tokens)
            oai["model"] = target_alias
            # Streaming is translated back non-streaming; Claude Code accepts
            # a complete message response.
            oai.pop("stream", None)

            wire.record("request", oai,
                        {"requested_model": requested,
                         "background": is_background, "upstream": target})
            headers = {"Content-Type": "application/json"}
            if upstream_api_key:
                # llama-server's --api-key expects a bearer token. Without
                # this, turning auth on upstream surfaces as a mid-campaign
                # 401 that looks like a model failure.
                headers["Authorization"] = "Bearer " + upstream_api_key
            try:
                upstream_resp = call_upstream(
                    gate, urllib.request.urlopen, target, headers, oai)
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
    ap.add_argument("--allow-session-titles", action="store_true",
                    help="send Claude Code's session-naming request to the "
                         "model instead of answering it here. Off by "
                         "default: it is not part of the agent loop, it is "
                         "charged to the run's wall clock, and its prompt "
                         "contaminates the next agent turn's KV cache.")
    ap.add_argument("--max-output-tokens", type=int, default=None,
                    help="clamp each turn's output budget; Claude Code asks "
                         "for 32000, which a local model can spend 22 minutes "
                         "on. Recorded with every run.")
    ap.add_argument("--background-upstream", default=None)
    ap.add_argument("--background-alias", default=None)
    # Read from the environment by default so the key never lands in argv,
    # where any other user's `ps` can read it - the same reasoning lls-sandbox
    # applies to LLS_BROKER_TOKEN. LLS_API_KEY is the fleet's own name for it
    # (lls prints that name when a model serves with --api-key); LLAMA_API_KEY
    # is llama.cpp's and is accepted as a fallback.
    ap.add_argument("--upstream-api-key",
                    default=os.environ.get("LLS_API_KEY")
                    or os.environ.get("LLAMA_API_KEY"))
    args = ap.parse_args()
    srv = make_server(args.port, args.upstream, args.alias, args.wire_log,
                      args.background_upstream, args.background_alias,
                      args.upstream_api_key, args.max_output_tokens,
                      suppress_session_titles=not args.allow_session_titles)
    print("shim listening on http://127.0.0.1:%d -> %s (%s)"
          % (srv.server_port, args.upstream, args.alias), flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
