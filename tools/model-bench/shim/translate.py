"""Anthropic Messages API <-> OpenAI Chat Completions translation.

Pure functions only. No I/O, no network, no logging - everything here is
directly unit-testable, which is the point: a lossy translation here is
indistinguishable from poor model quality once it reaches the benchmark.
"""

import json

# Keys that pass through unchanged when present. Absent keys stay absent:
# the server holds the card sampling, and inventing a default overrides it.
_PASSTHROUGH = ("max_tokens", "temperature", "top_p", "top_k", "stream")


def _system_to_text(system):
    """Claude Code sends `system` as either a string or a list of text blocks."""
    if system is None:
        return None
    if isinstance(system, str):
        return system
    return "\n\n".join(b.get("text", "") for b in system
                       if isinstance(b, dict) and b.get("type") == "text")


def _blocks_to_text(content):
    """Flatten a content value (string or block list) to plain text."""
    if isinstance(content, str):
        return content
    if not isinstance(content, list):
        return ""
    return "\n".join(b.get("text", "") for b in content
                     if isinstance(b, dict) and b.get("type") == "text")


def _translate_message(msg):
    """Translate one Anthropic message into one or more OpenAI messages.

    Returns a list because a single Anthropic user turn may carry both
    tool_result blocks and fresh text, which OpenAI must express separately.
    """
    role, content = msg["role"], msg.get("content")

    if isinstance(content, str):
        return [{"role": role, "content": content}]
    if not isinstance(content, list):
        return [{"role": role, "content": ""}]

    out, text_parts, tool_calls = [], [], []
    for block in content:
        if not isinstance(block, dict):
            continue
        btype = block.get("type")
        if btype == "text":
            text_parts.append(block.get("text", ""))
        elif btype == "tool_use":
            tool_calls.append({
                "id": block.get("id", ""),
                "type": "function",
                "function": {"name": block.get("name", ""),
                             "arguments": json.dumps(block.get("input", {}))},
            })
        elif btype == "tool_result":
            out.append({"role": "tool",
                        "tool_call_id": block.get("tool_use_id", ""),
                        "content": _blocks_to_text(block.get("content", ""))})

    if text_parts or tool_calls:
        m = {"role": role, "content": "\n".join(text_parts)}
        if tool_calls:
            m["tool_calls"] = tool_calls
        out.append(m)
    return out


def _translate_tools(tools):
    return [{"type": "function",
             "function": {"name": t["name"],
                          "description": t.get("description", ""),
                          "parameters": t.get("input_schema", {})}}
            for t in tools]


def anthropic_to_openai(body, max_output_tokens=None):
    """Translate an Anthropic /v1/messages request body to OpenAI form.

    `max_output_tokens` bounds a single turn. Claude Code asks for 32000
    every time, which is reasonable against Anthropic hardware and is not
    against a model decoding at ~24 tok/s: one response can run 22 minutes,
    and a model that fails to terminate spends the entire budget. Capping
    changes nothing for a model that stops on its own, so it bounds the
    pathological case without distorting the healthy one. Record the value
    with the run - it is a benchmark parameter, not an implementation detail.

    All system content is coalesced into a single leading message. Claude
    Code supplies a top-level `system`, and a SessionStart hook adds a
    further system-role message *after* the first user turn. Several chat
    templates in this fleet - Qwopus among them - call
    raise_exception('System message must be at the beginning'), which
    llama-server surfaces as a 500 on every request. Hoisting is also the
    faithful reading: hook context is session-level instruction, not a
    conversational turn, so it belongs with the system prompt rather than
    in the middle of the dialogue.
    """
    messages = []
    system_parts = []
    system_text = _system_to_text(body.get("system"))
    if system_text:
        system_parts.append(system_text)
    for msg in body.get("messages", []):
        if msg.get("role") == "system":
            text = _blocks_to_text(msg.get("content"))
            if text:
                system_parts.append(text)
            continue
        messages.extend(_translate_message(msg))
    if system_parts:
        messages.insert(0, {"role": "system",
                            "content": "\n\n".join(system_parts)})

    out = {"model": body.get("model"), "messages": messages}
    for key in _PASSTHROUGH:
        if key in body:
            out[key] = body[key]
    if "stop_sequences" in body:
        out["stop"] = body["stop_sequences"]
    if body.get("tools"):
        out["tools"] = _translate_tools(body["tools"])
    if max_output_tokens:
        out["max_tokens"] = min(out.get("max_tokens") or max_output_tokens,
                                max_output_tokens)
    return out


def count_tokens_estimate(body):
    """Rough token estimate for /v1/messages/count_tokens.

    llama-server exposes /tokenize, but Claude Code calls count_tokens far
    more often than it needs precision, and a round trip per call distorts
    the wall-clock measurement. ~4 chars/token, floor of 1.
    """
    chars = len(_system_to_text(body.get("system")) or "")
    for msg in body.get("messages", []):
        chars += len(_blocks_to_text(msg.get("content", "")))
    return max(1, chars // 4)
