#!/usr/bin/env python3
"""Synthetic-corpus generator: conformance fixtures -> verified training records.

Walks every `.bock` fixture under `compiler/tests/conformance/`, parses the
same `// TEST:` / `// EXPECT:` / `// FILE:` directive grammar the Rust test
harness uses (compiler/tests/harness/), and emits one JSONL record per fixture
bundling:

  * the Bock source (entry module + any auxiliary `// FILE:` modules),
  * the transpiled output for each shipping target (js, ts, python, rust, go),
    produced by `bock build -t <target> --source-only`,
  * the fixture's declared expected behavior (stdout text, diagnostic code at
    line:col, or "checks clean").

Verification performed at generation time (nothing lands in the corpus
unverified, and nothing is dropped silently):

  * execution / static fixtures: every per-target transpile must succeed
    (`bock build` runs the full frontend, so a successful build proves the
    source checks clean). Failures are recorded on the record
    (`status: "build_failed"` with stderr) and counted as pipeline failures.
  * diagnostic fixtures (declare `// EXPECT: error E<code> at <l>:<c>`):
    `bock check` is run against the original fixture file and must fail,
    surfacing every declared code and `line:col` — mirroring the assertions in
    compiler/tests/execution.rs. The captured compiler output is embedded in
    the record. Diagnostic fixtures are intentionally not transpiled (the
    source is invalid by design).

    Verification *scope* also mirrors the harness: execution.rs drives only
    `conformance/effects/` and `conformance/types-diagnostics/` through `bock
    check`, so declared-vs-actual drift in those dirs fails the run. A
    diagnostic fixture anywhere else carries a declaration no CI test
    enforces; drift there is surfaced as a manifest warning and the record is
    kept with `diagnostic.verified: false` — visible, never silently dropped,
    and excluded by consumers filtering on `verified`.

Cross-target *runtime* verification (actually executing the transpiled
programs on the five toolchains) is the conformance suite's job
(tools/scripts/run-conformance.sh); this generator deliberately does not
repeat it. See tools/corpus/README.md for the record schema and the OQ2
publication gate.

Usage:
    tools/corpus/generate.py [--repo-root DIR] [--bock-bin PATH]
                             [--out-dir DIR] [--jobs N] [--targets a,b,...]

Exit status: 0 on a fully-clean run; 1 if any fixture failed to load, any
build failed, or any diagnostic fixture did not verify. The corpus and
manifest are still written on failure (failed entries are marked, not
dropped) so the failure modes are inspectable.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime
import json
import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

SCHEMA_VERSION = "1.0.0"

# Stable ordering of the v1 shipping targets (mirrors TARGET_ORDER in
# compiler/tests/execution.rs).
TARGET_ORDER = ["js", "ts", "python", "rust", "go"]

# Top-level conformance categories whose `error E<code> at <l>:<c>`
# expectations the Rust harness actually enforces via `bock check` (see
# compiler/tests/execution.rs: `conformance_diagnostic_fixtures_check_as_declared`,
# which walks the ENTIRE conformance tree — every category is wired). Drift
# between a declared diagnostic and the live compiler in these dirs fails the
# pipeline; drift elsewhere — where no CI test enforces the declaration — is a
# manifest warning with the record kept as `verified: false`. Because the
# harness walks the whole tree, any NEW top-level category is harness-enforced
# from the moment it gains a diagnostic fixture: add it here in the same PR
# (lockstep with DIAGNOSTIC_FIXTURE_CATEGORIES in execution.rs).
HARNESS_WIRED_DIAGNOSTIC_CATEGORIES = {
    "context",
    "effects",
    "exec",
    "interp",
    "parse",
    "stdlib",
    "time",
    "types",
    "types-diagnostics",
}


# ---------------------------------------------------------------------------
# Directive parsing — mirrors compiler/tests/harness/{mod,expectation}.rs.
# ---------------------------------------------------------------------------


@dataclass
class Fixture:
    """A parsed conformance fixture (the Python analogue of harness TestCase)."""

    path: Path  # absolute path on disk
    rel: str  # path relative to the conformance root, '/'-separated
    name: str  # `// TEST:` value
    no_errors: bool = False
    output: str | None = None  # first `// EXPECT: output "..."`
    errors: list[dict] = field(default_factory=list)  # `error E<code> at l:c`
    allowed_targets: list[str] = field(default_factory=list)
    unrecognized: list[str] = field(default_factory=list)  # unknown EXPECT values
    entry_source: str = ""  # entry-module source (directives stripped)
    aux_files: list[tuple[str, str]] = field(default_factory=list)


class FixtureLoadError(Exception):
    """A fixture failed to parse (missing TEST directive, malformed EXPECT)."""


def parse_expectation(fx: Fixture, value: str, line_number: int) -> None:
    """Parse one `// EXPECT:` value into `fx`, mirroring expectation.rs.

    Unknown expectation types are recorded (not silently dropped) so directive
    typos surface in the manifest instead of vanishing.
    """
    value = value.strip()
    if value == "no_errors":
        fx.no_errors = True
        return
    if value.startswith("error "):
        rest = value[len("error ") :]
        parts = rest.split(" ", 2)
        if len(parts) == 3 and parts[1] == "at":
            loc = parts[2].split(":", 1)
            # ASCII-digit check mirrors the harness's u32 parse (and avoids
            # int() rejecting exotic Unicode digits that isdigit() accepts).
            def _is_num(s: str) -> bool:
                s = s.strip()
                return bool(s) and s.isascii() and s.isdigit()

            if len(loc) == 2 and _is_num(loc[0]) and _is_num(loc[1]):
                fx.errors.append(
                    {
                        "code": parts[0],
                        "line": int(loc[0]),
                        "col": int(loc[1]),
                    }
                )
                return
        raise FixtureLoadError(
            f"{fx.path}: line {line_number}: malformed error expectation {value!r}"
        )
    if value.startswith("output "):
        text = value[len("output ") :].strip()
        if text.startswith('"') and text.endswith('"') and len(text) >= 2:
            if fx.output is None:  # harness takes the first output directive
                fx.output = text[1:-1]
            return
        raise FixtureLoadError(
            f"{fx.path}: line {line_number}: malformed output expectation {value!r}"
        )
    if value.startswith("targets "):
        rest = value[len("targets ") :]
        ids = sorted({tok.strip() for tok in rest.split(",") if tok.strip()})
        if not ids:
            raise FixtureLoadError(
                f"{fx.path}: line {line_number}: malformed targets expectation {value!r}"
            )
        if not fx.allowed_targets:  # harness takes the first targets directive
            fx.allowed_targets = ids
        return
    # Unknown expectation type. The Rust harness ignores these for forward
    # compatibility; we keep them visible so typos don't silently strip a
    # fixture of its expectation.
    fx.unrecognized.append(value)


def parse_fixture(path: Path, conformance_root: Path) -> Fixture:
    """Parse a fixture file: directives at top, then source, then FILE sections."""
    content = path.read_text(encoding="utf-8")
    rel = path.relative_to(conformance_root).as_posix()
    fx = Fixture(path=path, rel=rel, name="")

    lines = content.splitlines(keepends=True)
    body_start = 0  # index of first non-directive line
    for idx, raw in enumerate(lines):
        trimmed = raw.strip()
        line_number = idx + 1
        if trimmed == "":
            body_start = idx + 1
            continue
        if trimmed.startswith("// TEST: "):
            fx.name = trimmed[len("// TEST: ") :].strip()
            body_start = idx + 1
            continue
        if trimmed.startswith("// EXPECT: "):
            parse_expectation(fx, trimmed[len("// EXPECT: ") :], line_number)
            body_start = idx + 1
            continue
        break  # first non-directive line: the rest is source

    if not fx.name:
        raise FixtureLoadError(f"{path}: missing `// TEST: <name>` directive")

    # Split the body into entry source and `// FILE: <relpath>` sections
    # (mirrors split_file_sections in harness/mod.rs).
    entry: list[str] = []
    aux: list[tuple[str, list[str]]] = []
    current: list[str] | None = None
    for raw in lines[body_start:]:
        stripped = raw.lstrip()
        if stripped.startswith("// FILE:"):
            relpath = stripped[len("// FILE:") :].strip()
            current = []
            aux.append((relpath, current))
            continue
        line = raw if raw.endswith("\n") else raw + "\n"
        (entry if current is None else current).append(line)

    fx.entry_source = "".join(entry)
    fx.aux_files = [(p, "".join(ls)) for p, ls in aux]
    return fx


def discover_fixtures(conformance_root: Path) -> list[Path]:
    """All `.bock` files under the conformance root, deterministically ordered."""
    return sorted(conformance_root.rglob("*.bock"))


# ---------------------------------------------------------------------------
# Per-fixture corpus generation.
# ---------------------------------------------------------------------------


def fixture_kind(fx: Fixture) -> str:
    """Classify how the fixture's expectation is verified.

    diagnostic  — declares `error E<code> at l:c`; verified via `bock check`
                  failing with that code/location. Not transpiled.
    execution   — declares `output "..."`; transpiled per target; runtime
                  behavior is pinned by the conformance execution suite.
    static      — everything else (no_errors or expectation-free); transpiled
                  per target; a successful build proves it checks clean.
    """
    if fx.errors:
        return "diagnostic"
    if fx.output is not None:
        return "execution"
    return "static"


def run_bock_check(bock_bin: Path, fixture_path: Path) -> dict:
    """Run `bock check` on the original fixture file, capturing the diagnostics.

    Diagnostic directives' `<line>:<col>` refer to the file as written on disk
    (including the leading directive comments), so — like the Rust harness —
    we check the original path, not the directive-stripped source.
    """
    proc = subprocess.run(
        [str(bock_bin), "check", str(fixture_path)],
        capture_output=True,
        text=True,
        timeout=120,
    )
    return {
        "command": f"bock check {fixture_path.name}",
        "exit_code": proc.returncode,
        "output": proc.stdout + proc.stderr,
    }


def collect_emitted_files(build_dir: Path) -> list[dict]:
    """All emitted files under build/<target>/, sourcemaps excluded."""
    files = []
    for p in sorted(build_dir.rglob("*")):
        if not p.is_file() or p.suffix == ".map":
            continue
        files.append(
            {
                "path": p.relative_to(build_dir).as_posix(),
                "content": p.read_text(encoding="utf-8"),
            }
        )
    return files


def transpile_fixture(
    fx: Fixture, bock_bin: Path, targets: list[str], scratch_root: Path
) -> dict[str, dict]:
    """Write the fixture into a temp project and `bock build --source-only` it
    for each target. Returns {target: per-target record entry}."""
    results: dict[str, dict] = {}
    project_dir = Path(
        tempfile.mkdtemp(prefix=fx.rel.replace("/", "_") + "-", dir=scratch_root)
    )
    try:
        (project_dir / "main.bock").write_text(fx.entry_source, encoding="utf-8")
        for rel, content in fx.aux_files:
            dest = project_dir / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(content, encoding="utf-8")

        for target in targets:
            proc = subprocess.run(
                [str(bock_bin), "build", "-t", target, "--source-only"],
                cwd=project_dir,
                capture_output=True,
                text=True,
                timeout=300,
            )
            if proc.returncode != 0:
                results[target] = {
                    "status": "build_failed",
                    "stderr": (proc.stdout + proc.stderr)[-4000:],
                }
                continue
            build_dir = project_dir / "build" / target
            results[target] = {
                "status": "ok",
                "files": collect_emitted_files(build_dir),
            }
    finally:
        shutil.rmtree(project_dir, ignore_errors=True)
    return results


def build_record(
    fx: Fixture, bock_bin: Path, targets: list[str], scratch_root: Path, commit: str
) -> tuple[dict, list[str], list[str]]:
    """Produce the corpus record for one fixture.

    Returns (record, failures, warnings): failures are run-failing
    verification problems; warnings are surfaced-but-non-fatal ones
    (diagnostic drift in a category the harness does not enforce). Both empty
    for a fully verified record.
    """
    kind = fixture_kind(fx)
    failures: list[str] = []
    warnings: list[str] = []

    record: dict = {
        "schema_version": SCHEMA_VERSION,
        "id": fx.rel.removesuffix(".bock"),
        "test_name": fx.name,
        "category": fx.rel.split("/", 1)[0],
        "fixture_path": f"compiler/tests/conformance/{fx.rel}",
        "kind": kind,
        "source": {
            "entry_path": "main.bock",
            "entry": fx.entry_source,
            "aux_files": [{"path": p, "content": c} for p, c in fx.aux_files],
        },
        "expected": {
            "output": fx.output,
            "no_errors": fx.no_errors,
            "errors": fx.errors,
            "allowed_targets": fx.allowed_targets or list(TARGET_ORDER),
            "unrecognized_directives": fx.unrecognized,
        },
        "bock_commit": commit,
    }

    if kind == "diagnostic":
        check = run_bock_check(bock_bin, fx.path)
        problems: list[str] = []
        verified = check["exit_code"] != 0
        for err in fx.errors:
            loc = f"{err['line']}:{err['col']}"
            if err["code"] not in check["output"] or loc not in check["output"]:
                verified = False
                problems.append(
                    f"{fx.rel}: `bock check` did not surface {err['code']} at {loc}"
                )
        if check["exit_code"] == 0:
            problems.append(f"{fx.rel}: diagnostic fixture checked clean")
        record["diagnostic"] = {**check, "verified": verified}
        record["targets"] = {}  # invalid-by-design source is not transpiled
        if record["category"] in HARNESS_WIRED_DIAGNOSTIC_CATEGORIES:
            # The conformance suite enforces these declarations in CI; drift
            # here means corpus and suite disagree — fail the run.
            failures.extend(problems)
        else:
            # No CI test drives this category through `bock check`; the
            # declaration is unenforced fixture metadata. Surface the drift
            # loudly, keep the record marked unverified.
            warnings.extend(
                f"{p} (declared diagnostic is not harness-enforced for "
                f"category {record['category']!r}; record kept with "
                "diagnostic.verified: false)"
                for p in problems
            )
        return record, failures, warnings

    # execution / static: transpile to every allowed target.
    allowed = [t for t in targets if t in record["expected"]["allowed_targets"]]
    record["targets"] = transpile_fixture(fx, bock_bin, allowed, scratch_root)
    for target, entry in record["targets"].items():
        if entry["status"] != "ok":
            failures.append(
                f"{fx.rel}: `bock build -t {target} --source-only` failed:\n"
                + entry.get("stderr", "")
            )
    return record, failures, warnings


# ---------------------------------------------------------------------------
# Driver.
# ---------------------------------------------------------------------------


def default_bock_bin(repo_root: Path) -> Path | None:
    """Resolve the `bock` binary: $BOCK_BIN, then $CARGO_TARGET_DIR/debug/bock,
    then the workspace-default target dir."""
    if env := os.environ.get("BOCK_BIN"):
        return Path(env)
    if env := os.environ.get("CARGO_TARGET_DIR"):
        return Path(env) / "debug" / "bock"
    return repo_root / "compiler" / "target" / "debug" / "bock"


def main() -> int:
    script_dir = Path(__file__).resolve().parent
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument(
        "--repo-root",
        type=Path,
        default=script_dir.parent.parent,
        help="Bock repo root (default: two levels above this script)",
    )
    ap.add_argument(
        "--bock-bin",
        type=Path,
        default=None,
        help="Path to the bock CLI (default: $BOCK_BIN, then "
        "$CARGO_TARGET_DIR/debug/bock, then compiler/target/debug/bock; "
        "build it with `cargo build -p bock --bin bock`)",
    )
    ap.add_argument(
        "--out-dir",
        type=Path,
        default=script_dir / "out",
        help="Output directory (default: tools/corpus/out — gitignored)",
    )
    ap.add_argument(
        "--targets",
        default=",".join(TARGET_ORDER),
        help=f"Comma-separated target ids (default: {','.join(TARGET_ORDER)})",
    )
    ap.add_argument(
        "--jobs",
        type=int,
        default=os.cpu_count() or 4,
        help="Parallel fixture workers (default: CPU count)",
    )
    args = ap.parse_args()

    repo_root = args.repo_root.resolve()
    conformance_root = repo_root / "compiler" / "tests" / "conformance"
    if not conformance_root.is_dir():
        print(f"error: no conformance dir at {conformance_root}", file=sys.stderr)
        return 1

    bock_bin = (args.bock_bin or default_bock_bin(repo_root)).resolve()
    if not bock_bin.is_file():
        print(
            f"error: bock binary not found at {bock_bin}\n"
            "hint: cargo build -p bock --bin bock   (then pass --bock-bin or "
            "set BOCK_BIN / CARGO_TARGET_DIR)",
            file=sys.stderr,
        )
        return 1

    targets = [t.strip() for t in args.targets.split(",") if t.strip()]
    unknown = [t for t in targets if t not in TARGET_ORDER]
    if unknown:
        print(f"error: unknown target ids: {', '.join(unknown)}", file=sys.stderr)
        return 1

    commit = subprocess.run(
        ["git", "-C", str(repo_root), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
    ).stdout.strip()
    version = subprocess.run(
        [str(bock_bin), "--version"], capture_output=True, text=True
    ).stdout.strip()

    # Honor the project convention: namespace /tmp scratch dirs per session.
    ns = os.environ.get("BOCK_TEST_NAMESPACE")
    scratch_root = Path(
        tempfile.mkdtemp(prefix=f"{ns}-corpus-" if ns else "bock-corpus-")
    )

    paths = discover_fixtures(conformance_root)
    print(f"discovered {len(paths)} fixtures under {conformance_root}")

    fixtures: list[Fixture] = []
    skipped: list[dict] = []  # load failures: {fixture, reason}
    for p in paths:
        try:
            fixtures.append(parse_fixture(p, conformance_root))
        except (FixtureLoadError, UnicodeDecodeError) as e:
            skipped.append(
                {"fixture": p.relative_to(conformance_root).as_posix(), "reason": str(e)}
            )

    records: dict[str, dict] = {}
    failures: list[str] = []
    warnings_by_id: dict[str, list[str]] = {}
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
            futs = {
                pool.submit(
                    build_record, fx, bock_bin, targets, scratch_root, commit
                ): fx
                for fx in fixtures
            }
            done = 0
            for fut in concurrent.futures.as_completed(futs):
                record, fx_failures, fx_warnings = fut.result()
                records[record["id"]] = record
                failures.extend(fx_failures)
                if fx_warnings:
                    warnings_by_id[record["id"]] = fx_warnings
                done += 1
                if done % 50 == 0 or done == len(fixtures):
                    print(f"  processed {done}/{len(fixtures)} fixtures")
    finally:
        shutil.rmtree(scratch_root, ignore_errors=True)
    failures.sort()  # as_completed order is nondeterministic; keep output stable

    # ---- write outputs -----------------------------------------------------
    out_dir = args.out_dir.resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    corpus_path = out_dir / "corpus.jsonl"
    ordered = [records[k] for k in sorted(records)]
    with corpus_path.open("w", encoding="utf-8") as f:
        for rec in ordered:
            f.write(json.dumps(rec, ensure_ascii=False, sort_keys=True) + "\n")

    # ---- stats + manifest ----------------------------------------------------
    by_kind: dict[str, int] = {}
    pair_counts = {"ok": 0, "build_failed": 0}
    per_target: dict[str, dict[str, int]] = {
        t: {"ok": 0, "build_failed": 0} for t in targets
    }
    diag = {"verified": 0, "unverified": 0}
    warnings: list[str] = []
    for rec in ordered:
        by_kind[rec["kind"]] = by_kind.get(rec["kind"], 0) + 1
        for target, entry in rec["targets"].items():
            pair_counts[entry["status"]] += 1
            per_target[target][entry["status"]] += 1
        if rec["kind"] == "diagnostic":
            diag["verified" if rec["diagnostic"]["verified"] else "unverified"] += 1
        for directive in rec["expected"]["unrecognized_directives"]:
            warnings.append(
                f"{rec['fixture_path']}: unrecognized EXPECT directive "
                f"{directive!r} (ignored by the harness too — possible typo)"
            )
        warnings.extend(warnings_by_id.get(rec["id"], []))

    manifest = {
        "schema_version": SCHEMA_VERSION,
        "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(
            timespec="seconds"
        ),
        "generator": "tools/corpus/generate.py",
        "bock_commit": commit,
        "bock_version": version,
        "targets": targets,
        "fixtures_discovered": len(paths),
        "fixtures_skipped": skipped,
        "records": len(ordered),
        "records_by_kind": by_kind,
        "fixture_target_pairs": pair_counts,
        "pairs_by_target": per_target,
        "diagnostics": diag,
        "failures": failures,
        "warnings": warnings,
    }
    manifest_path = out_dir / "manifest.json"
    manifest_path.write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )

    # ---- report -------------------------------------------------------------
    print()
    print("=== synthetic corpus summary ===")
    print(f"  fixtures discovered:    {len(paths)}")
    print(f"  fixtures skipped:       {len(skipped)}")
    for s in skipped:
        print(f"    - {s['fixture']}: {s['reason']}")
    print(f"  records written:        {len(ordered)}  -> {corpus_path}")
    print(f"  records by kind:        {by_kind}")
    print(
        f"  fixture-target pairs:   {pair_counts['ok']} ok, "
        f"{pair_counts['build_failed']} failed"
    )
    for t in targets:
        print(
            f"    {t:<7} {per_target[t]['ok']} ok, "
            f"{per_target[t]['build_failed']} failed"
        )
    print(
        f"  diagnostic fixtures:    {diag['verified']} verified, "
        f"{diag['unverified']} unverified"
    )
    for w in warnings:
        print(f"  warning: {w}")
    if failures:
        print(f"\n{len(failures)} failure(s):")
        for msg in failures:
            print(f"  - {msg}")
    print(f"  manifest:               {manifest_path}")

    return 1 if (failures or skipped) else 0


if __name__ == "__main__":
    sys.exit(main())
