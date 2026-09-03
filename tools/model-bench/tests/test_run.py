import json
import os
import shutil
import subprocess
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
    claude_argv,
    make_config_dir,
    child_env,
    parse_transcript,
    describe_tree_change,
    scrub_outside_paths,
    snapshot_tree,
    seed_defect,
    tree_fingerprint,
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
    require_key = None      # when set, mimic the data plane's --api-key guard
    seen_auth = None

    def do_GET(self):
        _ModelsHandler.seen_auth = self.headers.get("Authorization")
        if (_ModelsHandler.require_key and
                _ModelsHandler.seen_auth != "Bearer " + _ModelsHandler.require_key):
            self.send_response(401)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
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
        _ModelsHandler.require_key = None
        _ModelsHandler.seen_auth = None
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

    def test_authenticates_against_a_key_guarded_upstream(self):
        """The real data plane serves /v1/models behind --api-key.

        Every fleet model carries `--api-key cred:lls-data-plane`, so an
        unauthenticated guard 401s and aborts every run before it starts.
        The guard is the first thing each run does, so this failed closed
        on the whole campaign.
        """
        _ModelsHandler.require_key = "secret-key"
        _ModelsHandler.served = "qwopus-coder"
        self.assertEqual(
            assert_model_identity(self.base, "qwopus-coder",
                                  api_key="secret-key"),
            ["qwopus-coder"])
        self.assertEqual(_ModelsHandler.seen_auth, "Bearer secret-key")

    def test_no_authorization_header_when_no_key_is_configured(self):
        """An unguarded upstream must not receive a bare 'Bearer None'."""
        assert_model_identity(self.base, "qwopus-coder", api_key=None)
        self.assertIsNone(_ModelsHandler.seen_auth)


if __name__ == "__main__":
    unittest.main()


class TestClaudeInvocation(unittest.TestCase):
    def test_mcp_config_is_a_full_config_object(self):
        """CLI 2.1.252 rejects a bare '{}' and exits 1 before reaching the shim."""
        argv = claude_argv("do the thing", 40)
        cfg = json.loads(argv[argv.index("--mcp-config") + 1])
        self.assertEqual(cfg, {"mcpServers": {}})

    def test_prompt_and_turn_cap_are_passed_through(self):
        argv = claude_argv("do the thing", 12)
        self.assertIn("do the thing", argv)
        self.assertEqual(argv[argv.index("--max-turns") + 1], "12")


class TestBenchmarkSessionIsIsolated(unittest.TestCase):
    """The measured session must not inherit the measuring session's setup.

    The parent runs with plugins and a SessionStart hook (superpowers), which
    llama-server saw verbatim: the hook's instructions were prepended to the
    model's system prompt, and the model paid prefill for them. That is both a
    confound and a cost. Pointing CLAUDE_CONFIG_DIR at an empty directory
    drops user settings, plugins and hooks while leaving the pinned repo's own
    CLAUDE.md discovery intact - the repo is the subject, our setup is not.
    """

    BASE = {
        "PATH": "/usr/bin",
        "HOME": "/home/x",
        "CLAUDECODE": "1",
        "CLAUDE_CODE_CHILD_SESSION": "1",
        "CLAUDE_CODE_MESSAGING_TOKEN": "parent-plumbing",
        "CLAUDE_CODE_ENTRYPOINT": "cli",
        "CLAUDE_PID": "1234",
    }

    def _env(self, config_dir="/tmp/cfg"):
        return child_env(8787, "m", base=dict(self.BASE), config_dir=config_dir)

    def test_parent_session_plumbing_is_stripped(self):
        env = self._env()
        for k in ("CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION",
                  "CLAUDE_CODE_MESSAGING_TOKEN", "CLAUDE_CODE_ENTRYPOINT",
                  "CLAUDE_PID"):
            self.assertNotIn(k, env)

    def test_config_dir_is_pointed_at_the_benchmark_owned_directory(self):
        self.assertEqual(self._env()["CLAUDE_CONFIG_DIR"], "/tmp/cfg")

    def test_no_config_dir_leaves_the_variable_unset(self):
        self.assertNotIn("CLAUDE_CONFIG_DIR", self._env(config_dir=None))

    def test_benign_environment_still_survives(self):
        env = self._env()
        self.assertEqual(env["PATH"], "/usr/bin")
        self.assertEqual(env["HOME"], "/home/x")


class TestOutsidePathScrub(unittest.TestCase):
    """The scratch clone must not hand the model the live repo's path.

    A model reached outside the scratch tree on its first tool call. It
    did not invent the path: a checked-in CLAUDE.md can name absolute
    locations outside the tree, and Claude Code injects CLAUDE.md into
    the system prompt verbatim.
    """

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="scrubtest-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def _write(self, rel, text):
        path = os.path.join(self.tmp, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as fh:
            fh.write(text)
        return path

    def test_absolute_outside_path_is_rewritten_to_scratch(self):
        p = self._write("CLAUDE.md",
                        "run `git -C /srv/live-repo log`\n")
        n = scrub_outside_paths(self.tmp, ["/srv/live-repo"])
        self.assertEqual(n, 1)
        self.assertNotIn("/srv/live-repo", open(p).read())
        self.assertIn(self.tmp, open(p).read())

    def test_nested_claude_md_is_scrubbed_too(self):
        p = self._write("tracking/CLAUDE.md", "see /srv/live-repo\n")
        scrub_outside_paths(self.tmp, ["/srv/live-repo"])
        self.assertNotIn("/srv/live-repo", open(p).read())

    def test_longer_paths_scrubbed_before_prefixes(self):
        """bock-worktrees must not be left as <scratch>-worktrees."""
        p = self._write("CLAUDE.md",
                        "at /srv/live-repo-worktrees/x\n")
        scrub_outside_paths(self.tmp, ["/srv/live-repo",
                                       "/srv/live-repo-worktrees"])
        self.assertNotIn("/srv/live-repo", open(p).read())

    def test_clean_file_is_left_alone(self):
        p = self._write("CLAUDE.md", "nothing to see\n")
        self.assertEqual(scrub_outside_paths(self.tmp, ["/opt/x"]), 0)
        self.assertEqual(open(p).read(), "nothing to see\n")

    def test_non_claude_md_files_are_untouched(self):
        """Scrubbing source would corrupt the task; only prompt-injected
        files are in scope."""
        p = self._write("src/lib.rs", "// /srv/live-repo\n")
        scrub_outside_paths(self.tmp, ["/srv/live-repo"])
        self.assertIn("/srv/live-repo", open(p).read())


class TestProtectedTreeTripwire(unittest.TestCase):
    """Scoring reads the scratch clone, so an edit anywhere else is
    invisible to it. Fingerprint the live repo around the run instead."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="tripwire-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        subprocess.run(["git", "init", "-q"], cwd=self.tmp, check=True)
        subprocess.run(["git", "config", "user.email", "t@t"], cwd=self.tmp)
        subprocess.run(["git", "config", "user.name", "t"], cwd=self.tmp)
        with open(os.path.join(self.tmp, "a.txt"), "w") as fh:
            fh.write("one\n")
        subprocess.run(["git", "add", "-A"], cwd=self.tmp, check=True)
        subprocess.run(["git", "commit", "-qm", "init"], cwd=self.tmp,
                       check=True)

    def test_unchanged_tree_fingerprints_identically(self):
        self.assertEqual(tree_fingerprint(self.tmp),
                         tree_fingerprint(self.tmp))

    def test_modified_tracked_file_changes_fingerprint(self):
        before = tree_fingerprint(self.tmp)
        with open(os.path.join(self.tmp, "a.txt"), "w") as fh:
            fh.write("two\n")
        self.assertNotEqual(before, tree_fingerprint(self.tmp))

    def test_new_untracked_file_changes_fingerprint(self):
        before = tree_fingerprint(self.tmp)
        with open(os.path.join(self.tmp, "b.txt"), "w") as fh:
            fh.write("new\n")
        self.assertNotEqual(before, tree_fingerprint(self.tmp))

    def test_new_commit_changes_fingerprint(self):
        before = tree_fingerprint(self.tmp)
        with open(os.path.join(self.tmp, "a.txt"), "w") as fh:
            fh.write("two\n")
        subprocess.run(["git", "commit", "-aqm", "x"], cwd=self.tmp,
                       check=True)
        self.assertNotEqual(before, tree_fingerprint(self.tmp))

    def test_missing_tree_is_reported_not_crashed(self):
        self.assertIsNotNone(tree_fingerprint("/nonexistent-path-xyz"))


class TestUnconfinedInvocation(unittest.TestCase):
    """`--run-as-user` is gone; nothing may reintroduce a privilege prefix.

    The flag could never be set up from inside this container, so it was
    always unset - an available-looking safeguard that documented a
    boundary nobody had. These tests pin the invocation as plainly
    unconfined so that fact stays visible in the code rather than being
    re-hidden behind an opt-in nobody can opt into.
    """

    def test_argv_is_the_cli_itself_with_no_privilege_prefix(self):
        argv = claude_argv("p", 40)
        self.assertEqual(argv[0], "claude")
        self.assertNotIn("sudo", argv)

    def test_claude_argv_takes_no_confinement_argument(self):
        import inspect
        params = inspect.signature(claude_argv).parameters
        self.assertEqual(list(params), ["prompt", "max_turns"])


class TestProtectValidation(unittest.TestCase):
    """A protected path that is not a git tree fingerprints to a constant,
    so the tripwire silently never fires. That must be a startup error."""

    def test_non_git_dir_is_detected(self):
        tmp = tempfile.mkdtemp(prefix="notgit-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        self.assertTrue(
            tree_fingerprint(tmp).startswith("NOT-A-GIT-TREE:"))

    def test_absent_dir_is_detected(self):
        self.assertTrue(
            tree_fingerprint("/nope-xyz").startswith("ABSENT:"))


class TestTreeChangeDetail(unittest.TestCase):
    """A tripwire that reports only THAT something changed is not
    diagnosable. A real false positive — merging into the protected
    checkout during a run — was indistinguishable from the model writing
    there, because both only moved the fingerprint."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="detail-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        for c in (["git", "init", "-q"], ["git", "config", "user.email", "t@t"],
                  ["git", "config", "user.name", "t"]):
            subprocess.run(c, cwd=self.tmp, check=False)
        self._write("a.txt", "one\n")
        subprocess.run(["git", "add", "-A"], cwd=self.tmp, check=True)
        subprocess.run(["git", "commit", "-qm", "init"], cwd=self.tmp, check=True)

    def _write(self, name, text):
        with open(os.path.join(self.tmp, name), "w") as fh:
            fh.write(text)

    def test_head_move_alone_is_labelled_as_such(self):
        before = snapshot_tree(self.tmp)
        self._write("a.txt", "two\n")
        subprocess.run(["git", "commit", "-aqm", "second"], cwd=self.tmp, check=True)
        d = describe_tree_change(self.tmp, before, snapshot_tree(self.tmp))
        self.assertTrue(d["changed"])
        self.assertTrue(d["head_moved"])
        self.assertTrue(d["head_moved_only"])
        self.assertNotEqual(d["head_before"], d["head_after"])
        self.assertEqual(d["worktree_paths"], [])

    def test_worktree_edit_is_reported_with_its_paths(self):
        before = snapshot_tree(self.tmp)
        self._write("a.txt", "dirty\n")
        d = describe_tree_change(self.tmp, before, snapshot_tree(self.tmp))
        self.assertTrue(d["changed"])
        self.assertFalse(d["head_moved"])
        self.assertFalse(d["head_moved_only"])
        self.assertIn("a.txt", d["worktree_paths"])

    def test_new_untracked_file_is_reported(self):
        before = snapshot_tree(self.tmp)
        self._write("b.txt", "new\n")
        d = describe_tree_change(self.tmp, before, snapshot_tree(self.tmp))
        self.assertIn("b.txt", d["worktree_paths"])

    def test_unchanged_tree_reports_no_change(self):
        before = snapshot_tree(self.tmp)
        d = describe_tree_change(self.tmp, before, snapshot_tree(self.tmp))
        self.assertFalse(d["changed"])
        self.assertEqual(d["worktree_paths"], [])

    def test_fingerprint_still_agrees_with_snapshot(self):
        """tree_fingerprint stays the equality primitive."""
        a = tree_fingerprint(self.tmp)
        self._write("a.txt", "x\n")
        self.assertNotEqual(a, tree_fingerprint(self.tmp))

    def test_detail_survives_a_vanished_tree(self):
        """The tripwire must never be the thing that kills a run."""
        before = snapshot_tree(self.tmp)
        shutil.rmtree(self.tmp)
        d = describe_tree_change(self.tmp, before, snapshot_tree(self.tmp))
        self.assertTrue(d["changed"])
