#!/usr/bin/env python3
"""Run the contract tests with a reported, replayable proptest seed.

Usage:
    python3 scripts/proptest_seed.py                 # random seed, printed and persisted
    python3 scripts/proptest_seed.py --seed 12345    # replay a previous run
    python3 scripts/proptest_seed.py --replay        # replay the seed saved in the report

The seed is exported as PROPTEST_RNG_SEED, printed up front, and written with
any minimal failing cases to target/proptest-report/report.json.
"""
import argparse
import json
import os
import re
import secrets
import subprocess
import sys
from pathlib import Path

REPORT_DIR = Path("target") / "proptest-report"
REPORT_FILE = REPORT_DIR / "report.json"
MAX_SEED = 2**64

FAIL_RE = re.compile(r"minimal failing input:\s*(.+)")
SHRINK_RE = re.compile(r"^cc [0-9a-f]+ # shrinks to (.+)$")
TEST_RE = re.compile(r"^---- (\S+) stdout ----")


def parse_seed(value):
    """Parse a seed argument; must be an unsigned 64-bit integer."""
    seed = int(value, 10)
    if not 0 <= seed < MAX_SEED:
        raise ValueError("seed must be in 0..2^64-1")
    return seed


def new_seed():
    return secrets.randbelow(MAX_SEED)


def extract_failures(output):
    """Return [{'test': name, 'minimal_failing_input': text}] from test output."""
    failures = []
    current = None
    for line in output.splitlines():
        match = TEST_RE.match(line.strip())
        if match:
            current = match.group(1)
            continue
        match = FAIL_RE.search(line) or SHRINK_RE.match(line.strip())
        if match:
            failures.append(
                {"test": current, "minimal_failing_input": match.group(1).strip()}
            )
    return failures


def build_report(seed, exit_code, output):
    return {
        "seed": seed,
        "exit_code": exit_code,
        "failures": extract_failures(output),
        "replay": f"python3 scripts/proptest_seed.py --seed {seed}",
    }


def write_report(report, path=REPORT_FILE):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--seed", type=parse_seed, help="seed to use")
    group.add_argument("--replay", action="store_true", help="reuse saved seed")
    parser.add_argument("cargo_args", nargs="*", help="extra args after --")
    args = parser.parse_args(argv)

    if args.replay:
        try:
            seed = json.loads(REPORT_FILE.read_text(encoding="utf-8"))["seed"]
        except (OSError, KeyError, ValueError):
            print(f"no saved seed in {REPORT_FILE}", file=sys.stderr)
            return 2
    else:
        seed = args.seed if args.seed is not None else new_seed()

    print(f"PROPTEST_RNG_SEED={seed}", flush=True)
    env = dict(os.environ, PROPTEST_RNG_SEED=str(seed))
    # Persist the seed before running so it survives a crash or timeout.
    write_report(build_report(seed, None, ""))

    proc = subprocess.run(
        ["cargo", "test", "--workspace", *args.cargo_args],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    sys.stdout.write(proc.stdout)
    report = build_report(seed, proc.returncode, proc.stdout)
    write_report(report)
    if proc.returncode != 0:
        print(f"\nproptest seed: {seed}")
        for item in report["failures"]:
            print(f"minimal failing case [{item['test']}]: {item['minimal_failing_input']}")
        print(f"replay with: {report['replay']}")
        print(f"report saved to {REPORT_FILE}")
    return proc.returncode


if __name__ == "__main__":
    sys.exit(main())
