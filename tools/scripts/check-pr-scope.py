#!/usr/bin/env python3
"""Check that a PR's changed files fall inside the scope it declared.

Sessions declare owned files in their session prompt, but nothing
mechanically enforces that declaration -- it is honour-system. This is
the mechanical form of the `scope_violations` axis in
`tools/model-bench/harness/score.py`, applied to real PRs instead of
benchmark runs. The matching semantics are deliberately the same as
that module's `in_scope()` (fnmatch globs), so a file judged in scope
by the benchmark is judged in scope here too.

Scope is declared as an `Owned-Files:` block in the PR body:

    Owned-Files:
    - tools/model-bench/
    - .github/workflows/scope-check.yml
    - compiler/crates/bock-lexer/src/*.rs

A trailing `/` means "that directory and everything under it". Entries
may be globs. The block ends at the first line that is not a list item
(or a blank line inside the block is tolerated).

INFORMATIONAL: `main()` reports violations and still exits 0. See
`.github/workflows/scope-check.yml` for the ratchet instructions.

Python 3 standard library only, matching `tools/model-bench`.
"""

import argparse
import fnmatch
import os
import sys

# List-item bullets accepted inside an Owned-Files block.
_BULLETS = ("- ", "* ", "+ ")

# Files that no PR needs to declare: they are the mechanical residue of
# a change rather than part of its scope.
DEFAULT_EXEMPT = (
    "Cargo.lock",
)


def parse_owned_files(body):
    """Extract the declared scope globs from a PR body.

    Returns a list of glob strings, empty if the body declares no
    `Owned-Files:` block. Matching of the header is case-insensitive and
    tolerates surrounding markdown emphasis/backticks, because PR bodies
    are written by hand.
    """
    if not body:
        return []

    globs = []
    in_block = False
    for raw in body.splitlines():
        line = raw.strip()
        if not in_block:
            # Tolerate markdown emphasis/backticks around the header.
            line = line.strip("*_`").strip()
            lowered = line.lower()
            if lowered.rstrip(":").rstrip() == "owned-files":
                in_block = True
            elif lowered.startswith("owned-files:"):
                # Inline form: `Owned-Files: a, b` on the header line.
                in_block = True
                for entry in line.split(":", 1)[1].split(","):
                    entry = entry.strip().strip("`").strip()
                    if entry:
                        globs.append(entry)
            continue

        if not line:
            # A blank line inside the block is tolerated; the block ends
            # at the first non-blank, non-list line.
            continue
        if not line.startswith(_BULLETS):
            break

        entry = line[2:].strip().strip("`").strip()
        if entry:
            globs.append(entry)

    return globs


def in_scope(path, allowed):
    """True if `path` matches any allowed entry.

    Semantics, in order:
      * exact string equality;
      * `fnmatch` glob (same as `harness.score.in_scope`);
      * directory prefix, for entries ending in `/` -- and also for
        entries that name a directory without the slash, since a PR
        author writing `tools/scripts` plainly means its contents.
    """
    for pat in allowed:
        if path == pat or fnmatch.fnmatch(path, pat):
            return True
        prefix = pat if pat.endswith("/") else pat + "/"
        if path.startswith(prefix):
            return True
        # A directory glob such as `compiler/crates/bock-*/` should also
        # cover everything beneath a matching directory.
        if "*" in pat or "?" in pat or "[" in pat:
            head = prefix.rstrip("/")
            parts = path.split("/")
            for i in range(1, len(parts)):
                if fnmatch.fnmatch("/".join(parts[:i]), head):
                    return True
    return False


def is_exempt(path, exempt=DEFAULT_EXEMPT):
    """True if `path` is scope-exempt (basename or full-path match)."""
    return any(path == e or os.path.basename(path) == e for e in exempt)


def find_violations(changed_files, allowed, exempt=DEFAULT_EXEMPT):
    """Changed files that fall outside the declared scope.

    An empty `allowed` means no scope was declared; the caller decides
    what that means (this returns no violations rather than flagging
    every file).
    """
    if not allowed:
        return []
    return [f for f in changed_files
            if not is_exempt(f, exempt) and not in_scope(f, allowed)]


def should_skip(actor):
    """True for actors whose PRs are not session-scoped (dependabot)."""
    return "dependabot" in (actor or "").lower()


def render_report(changed_files, allowed, violations, skipped=None):
    """Human-readable markdown summary for the CI job summary."""
    lines = ["## PR scope check", ""]

    if skipped:
        lines += ["Skipped: %s." % skipped, ""]
        return "\n".join(lines)

    if not allowed:
        lines += [
            "**No scope declared** -- the PR body has no `Owned-Files:` "
            "block, so there is nothing to check against. Passing.",
            "",
            "To declare scope, add to the PR body:",
            "",
            "```",
            "Owned-Files:",
            "- path/to/dir/",
            "- path/to/file.rs",
            "```",
            "",
        ]
        return "\n".join(lines)

    lines += ["Declared scope (%d entr%s):" % (
        len(allowed), "y" if len(allowed) == 1 else "ies"), ""]
    lines += ["- `%s`" % g for g in allowed]
    lines += ["", "Changed files: %d." % len(changed_files), ""]

    if not violations:
        lines += ["**In scope.** Every changed file matches a declared "
                  "entry.", ""]
    else:
        lines += ["**%d file%s outside the declared scope:**" % (
            len(violations), "" if len(violations) == 1 else "s"), ""]
        lines += ["- `%s`" % f for f in violations]
        lines += [
            "",
            "This job is **informational**: it does not fail the PR. Either "
            "add these paths to the `Owned-Files:` block if they belong to "
            "this change, or move them to a separate PR.",
            "",
        ]

    return "\n".join(lines)


def _read_changed_files(path):
    if path == "-":
        text = sys.stdin.read()
    else:
        with open(path, encoding="utf-8") as fh:
            text = fh.read()
    return [ln.strip() for ln in text.splitlines() if ln.strip()]


def _read_body(args):
    if args.body_file:
        with open(args.body_file, encoding="utf-8") as fh:
            return fh.read()
    return args.body or ""


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--changed-files", required=True,
                        help="file with one changed path per line, or '-'")
    parser.add_argument("--body", help="PR body text")
    parser.add_argument("--body-file", help="file containing the PR body")
    parser.add_argument("--actor", default="",
                        help="PR author login (dependabot PRs are skipped)")
    parser.add_argument("--summary-file",
                        help="append the markdown report here too")
    parser.add_argument("--strict", action="store_true",
                        help="exit 1 on violations (the ratchet switch)")
    args = parser.parse_args(argv)

    if should_skip(args.actor):
        report = render_report([], [], [], skipped="dependabot PR")
        violations = []
    else:
        changed = _read_changed_files(args.changed_files)
        allowed = parse_owned_files(_read_body(args))
        violations = find_violations(changed, allowed)
        report = render_report(changed, allowed, violations)

    print(report)
    if args.summary_file:
        with open(args.summary_file, "a", encoding="utf-8") as fh:
            fh.write(report + "\n")

    if violations and args.strict:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
