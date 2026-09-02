"""Unit tests for tools/scripts/check-pr-scope.py.

Run with:

    python3 -m unittest discover -s tools/scripts/tests

The module under test has a hyphen in its filename (it is a CLI script,
not an importable package), so it is loaded by path.
"""

import importlib.util
import os
import sys
import tempfile
import unittest

_SCRIPT = os.path.join(os.path.dirname(__file__), "..", "check-pr-scope.py")
_spec = importlib.util.spec_from_file_location("check_pr_scope", _SCRIPT)
scope = importlib.util.module_from_spec(_spec)
sys.modules["check_pr_scope"] = scope
_spec.loader.exec_module(scope)


class TestInScope(unittest.TestCase):
    def test_exact_match(self):
        self.assertTrue(scope.in_scope(
            ".github/workflows/scope-check.yml",
            [".github/workflows/scope-check.yml"]))

    def test_directory_prefix_with_trailing_slash(self):
        self.assertTrue(scope.in_scope(
            "tools/model-bench/harness/score.py", ["tools/model-bench/"]))

    def test_directory_prefix_without_trailing_slash(self):
        self.assertTrue(scope.in_scope(
            "tools/scripts/check-pr-scope.py", ["tools/scripts"]))

    def test_glob_matches(self):
        self.assertTrue(scope.in_scope(
            "stdlib/core/option/option.bock", ["stdlib/core/*/*.bock"]))

    def test_directory_glob_covers_subtree(self):
        self.assertTrue(scope.in_scope(
            "compiler/crates/bock-lexer/src/lib.rs",
            ["compiler/crates/bock-*/"]))

    def test_unrelated_file_is_out_of_scope(self):
        self.assertFalse(scope.in_scope(
            "CLAUDE.md", ["tools/scripts/check-pr-scope.py"]))

    def test_sibling_directory_is_not_a_prefix_match(self):
        # `tools/scripts-old/x` must not match the entry `tools/scripts`.
        self.assertFalse(scope.in_scope(
            "tools/scripts-old/x.sh", ["tools/scripts"]))

    def test_agrees_with_model_bench_semantics(self):
        # Same glob semantics as harness.score.in_scope for plain globs.
        self.assertTrue(scope.in_scope("a/b.rs", ["a/*.rs"]))
        self.assertFalse(scope.in_scope("a/b.rs", ["a/*.py"]))


class TestParseOwnedFiles(unittest.TestCase):
    def test_parses_block(self):
        body = (
            "Adds a scope check.\n\n"
            "Owned-Files:\n"
            "- tools/scripts/check-pr-scope.py\n"
            "- .github/workflows/scope-check.yml\n\n"
            "Some trailing prose.\n"
        )
        self.assertEqual(scope.parse_owned_files(body), [
            "tools/scripts/check-pr-scope.py",
            ".github/workflows/scope-check.yml",
        ])

    def test_block_ends_at_first_non_list_line(self):
        body = "Owned-Files:\n- a/\nProse here.\n- b/\n"
        self.assertEqual(scope.parse_owned_files(body), ["a/"])

    def test_accepts_markdown_emphasis_and_backticks(self):
        body = "**Owned-Files:**\n- `tools/scripts/`\n"
        self.assertEqual(scope.parse_owned_files(body), ["tools/scripts/"])

    def test_accepts_star_and_plus_bullets(self):
        body = "Owned-Files:\n* a/\n+ b/\n"
        self.assertEqual(scope.parse_owned_files(body), ["a/", "b/"])

    def test_inline_form(self):
        body = "Owned-Files: a/, b/\n"
        self.assertEqual(scope.parse_owned_files(body), ["a/", "b/"])

    def test_empty_declaration(self):
        self.assertEqual(scope.parse_owned_files("No scope here.\n"), [])
        self.assertEqual(scope.parse_owned_files(""), [])
        self.assertEqual(scope.parse_owned_files(None), [])

    def test_header_with_no_entries_is_empty(self):
        self.assertEqual(scope.parse_owned_files("Owned-Files:\n\nProse"), [])


class TestFindViolations(unittest.TestCase):
    def test_all_in_scope(self):
        self.assertEqual(scope.find_violations(
            ["tools/scripts/check-pr-scope.py"], ["tools/scripts/"]), [])

    def test_violation_is_reported(self):
        self.assertEqual(scope.find_violations(
            ["tools/scripts/check-pr-scope.py", "CLAUDE.md"],
            ["tools/scripts/"]), ["CLAUDE.md"])

    def test_empty_declaration_flags_nothing(self):
        self.assertEqual(scope.find_violations(["CLAUDE.md"], []), [])

    def test_cargo_lock_is_exempt(self):
        self.assertEqual(scope.find_violations(
            ["Cargo.lock"], ["tools/scripts/"]), [])


class TestSkip(unittest.TestCase):
    def test_dependabot_actor_is_skipped(self):
        self.assertTrue(scope.should_skip("dependabot[bot]"))
        self.assertTrue(scope.should_skip("Dependabot"))

    def test_human_actor_is_not_skipped(self):
        self.assertFalse(scope.should_skip("some-engineer"))
        self.assertFalse(scope.should_skip(""))
        self.assertFalse(scope.should_skip(None))


class TestRenderReport(unittest.TestCase):
    def test_no_scope_declared_message(self):
        out = scope.render_report(["a.rs"], [], [])
        self.assertIn("No scope declared", out)

    def test_violation_listed_and_marked_informational(self):
        out = scope.render_report(["a.rs"], ["b/"], ["a.rs"])
        self.assertIn("`a.rs`", out)
        self.assertIn("informational", out)

    def test_skip_message(self):
        out = scope.render_report([], [], [], skipped="dependabot PR")
        self.assertIn("Skipped", out)


class TestMain(unittest.TestCase):
    def _run(self, changed, body, extra=None):
        with tempfile.TemporaryDirectory() as tmp:
            cf = os.path.join(tmp, "changed.txt")
            with open(cf, "w", encoding="utf-8") as fh:
                fh.write("\n".join(changed) + "\n")
            argv = ["--changed-files", cf, "--body", body]
            argv += extra or []
            return scope.main(argv)

    def test_informational_exit_zero_on_violation(self):
        self.assertEqual(
            self._run(["CLAUDE.md"], "Owned-Files:\n- tools/\n"), 0)

    def test_strict_exit_one_on_violation(self):
        self.assertEqual(
            self._run(["CLAUDE.md"], "Owned-Files:\n- tools/\n", ["--strict"]),
            1)

    def test_strict_exit_zero_when_clean(self):
        self.assertEqual(
            self._run(["tools/x"], "Owned-Files:\n- tools/\n", ["--strict"]),
            0)

    def test_dependabot_skips_even_in_strict_mode(self):
        self.assertEqual(
            self._run(["CLAUDE.md"], "Owned-Files:\n- tools/\n",
                      ["--strict", "--actor", "dependabot[bot]"]), 0)

    def test_no_declaration_passes_in_strict_mode(self):
        self.assertEqual(
            self._run(["CLAUDE.md"], "no block here", ["--strict"]), 0)


if __name__ == "__main__":
    unittest.main()
