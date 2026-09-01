import json
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from shim.translate import anthropic_to_openai, count_tokens_estimate  # noqa: E402


class TestSystemPrompt(unittest.TestCase):
    def test_system_string_becomes_first_message(self):
        out = anthropic_to_openai({
            "model": "qwopus-coder",
            "system": "You are Claude Code.",
            "messages": [{"role": "user", "content": "hi"}],
        })
        self.assertEqual(out["messages"][0],
                         {"role": "system", "content": "You are Claude Code."})
        self.assertEqual(out["messages"][1], {"role": "user", "content": "hi"})

    def test_system_block_list_is_joined(self):
        # Claude Code sends `system` as a list of text blocks, not a string.
        out = anthropic_to_openai({
            "model": "m",
            "system": [{"type": "text", "text": "A"},
                       {"type": "text", "text": "B"}],
            "messages": [{"role": "user", "content": "hi"}],
        })
        self.assertEqual(out["messages"][0]["content"], "A\n\nB")


class TestContentBlocks(unittest.TestCase):
    def test_user_text_blocks_flatten_to_string(self):
        out = anthropic_to_openai({"model": "m", "messages": [
            {"role": "user", "content": [{"type": "text", "text": "hello"}]}]})
        self.assertEqual(out["messages"][0]["content"], "hello")

    def test_tool_use_becomes_openai_tool_call(self):
        out = anthropic_to_openai({"model": "m", "messages": [
            {"role": "assistant", "content": [
                {"type": "text", "text": "editing"},
                {"type": "tool_use", "id": "tu_1", "name": "Edit",
                 "input": {"file_path": "/a.rs", "old_string": "x",
                           "new_string": "y"}}]}]})
        m = out["messages"][0]
        self.assertEqual(m["role"], "assistant")
        self.assertEqual(m["content"], "editing")
        tc = m["tool_calls"][0]
        self.assertEqual(tc["id"], "tu_1")
        self.assertEqual(tc["type"], "function")
        self.assertEqual(tc["function"]["name"], "Edit")
        # Arguments MUST be a JSON string, and must round-trip losslessly.
        self.assertEqual(json.loads(tc["function"]["arguments"]),
                         {"file_path": "/a.rs", "old_string": "x",
                          "new_string": "y"})

    def test_tool_result_becomes_tool_role_message(self):
        out = anthropic_to_openai({"model": "m", "messages": [
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_1",
                 "content": "ok"}]}]})
        self.assertEqual(out["messages"][0],
                         {"role": "tool", "tool_call_id": "tu_1",
                          "content": "ok"})

    def test_tool_result_with_block_list_content(self):
        out = anthropic_to_openai({"model": "m", "messages": [
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_2",
                 "content": [{"type": "text", "text": "line1"},
                             {"type": "text", "text": "line2"}]}]}]})
        self.assertEqual(out["messages"][0]["content"], "line1\nline2")

    def test_mixed_tool_result_and_text_splits_into_two_messages(self):
        # A single Anthropic user turn can carry a tool_result AND new text.
        # OpenAI cannot express that in one message.
        out = anthropic_to_openai({"model": "m", "messages": [
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_1", "content": "ok"},
                {"type": "text", "text": "now do the next thing"}]}]})
        self.assertEqual(len(out["messages"]), 2)
        self.assertEqual(out["messages"][0]["role"], "tool")
        self.assertEqual(out["messages"][1],
                         {"role": "user", "content": "now do the next thing"})


class TestToolsAndParams(unittest.TestCase):
    def test_tool_definitions_translate(self):
        out = anthropic_to_openai({"model": "m", "messages": [], "tools": [
            {"name": "Read", "description": "Read a file",
             "input_schema": {"type": "object",
                              "properties": {"file_path": {"type": "string"}},
                              "required": ["file_path"]}}]})
        t = out["tools"][0]
        self.assertEqual(t["type"], "function")
        self.assertEqual(t["function"]["name"], "Read")
        self.assertEqual(t["function"]["description"], "Read a file")
        self.assertEqual(t["function"]["parameters"]["required"], ["file_path"])

    def test_sampling_and_stream_pass_through(self):
        out = anthropic_to_openai({"model": "m", "messages": [],
                                   "max_tokens": 4096, "temperature": 0.7,
                                   "top_p": 0.95, "stream": True,
                                   "stop_sequences": ["END"]})
        self.assertEqual(out["max_tokens"], 4096)
        self.assertEqual(out["temperature"], 0.7)
        self.assertEqual(out["top_p"], 0.95)
        self.assertTrue(out["stream"])
        self.assertEqual(out["stop"], ["END"])

    def test_absent_sampling_keys_are_omitted_not_defaulted(self):
        # Sampling is set per model card on the server. The shim must not
        # invent values, or it silently overrides the card.
        out = anthropic_to_openai({"model": "m", "messages": []})
        self.assertNotIn("temperature", out)
        self.assertNotIn("top_p", out)

    def test_count_tokens_estimate_is_monotonic(self):
        small = count_tokens_estimate({"messages": [
            {"role": "user", "content": "hi"}]})
        large = count_tokens_estimate({"messages": [
            {"role": "user", "content": "hi " * 1000}]})
        self.assertGreater(large, small)
        self.assertGreater(small, 0)


if __name__ == "__main__":
    unittest.main()


class TestSystemMessagesAreCoalesced(unittest.TestCase):
    """Templates in this family raise if a system message is not first.

    Claude Code sends a top-level `system` AND, when a SessionStart hook
    contributes context, a further system-role message after the first user
    turn. Qwopus' template calls raise_exception('System message must be at
    the beginning'), which llama-server returns as a 500 - so every request
    failed and the run recorded zero turns.
    """

    def test_trailing_system_message_is_hoisted_into_the_leading_one(self):
        out = anthropic_to_openai({
            "model": "m",
            "system": "primary",
            "messages": [
                {"role": "user", "content": "do the thing"},
                {"role": "system", "content": "hook context"},
                {"role": "assistant", "content": "ok"},
            ],
        })
        roles = [m["role"] for m in out["messages"]]
        self.assertEqual(roles, ["system", "user", "assistant"])
        self.assertEqual(out["messages"][0]["content"], "primary\n\nhook context")

    def test_system_message_with_no_top_level_system_still_leads(self):
        out = anthropic_to_openai({
            "model": "m",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "system", "content": "late"},
            ],
        })
        self.assertEqual([m["role"] for m in out["messages"]], ["system", "user"])
        self.assertEqual(out["messages"][0]["content"], "late")

    def test_ordering_of_non_system_turns_is_untouched(self):
        out = anthropic_to_openai({
            "model": "m",
            "system": "s",
            "messages": [
                {"role": "user", "content": "one"},
                {"role": "assistant", "content": "two"},
                {"role": "user", "content": "three"},
            ],
        })
        self.assertEqual([m["role"] for m in out["messages"]],
                         ["system", "user", "assistant", "user"])
        self.assertEqual([m["content"] for m in out["messages"][1:]],
                         ["one", "two", "three"])

    def test_no_system_content_means_no_system_message(self):
        out = anthropic_to_openai({"model": "m",
                                   "messages": [{"role": "user", "content": "hi"}]})
        self.assertEqual([m["role"] for m in out["messages"]], ["user"])
