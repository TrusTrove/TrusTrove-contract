"""Tests for scripts/invariant_report.sh (issue #1210).

The script itself is a thin bash wrapper around `stellar contract invoke` and
a few text-mangling checks. These tests exercise the pieces that *can* be
tested without a live chain:

* successful path — a stubbed `stellar` CLI that returns well-formed values
  makes the whole script exit 0 with an "All invariants hold." summary;
* invalid-input rejection — missing env vars or a missing .env file exit 2
  before any invoke happens;
* idempotency — two consecutive runs produce identical summaries; the script
  only ever reads chain state, so repeated runs must never diverge.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "invariant_report.sh"


STUB_ENV = {
    "DEPLOYER_ACCOUNT":        "GDEPLOYER",
    "REGISTRY_CONTRACT_ID":    "CREGISTRY",
    "INVOICE_CONTRACT_ID":     "CINVOICE",
    "POOL_USDC_CONTRACT_ID":   "CPOOLUSDC",
    "POOL_XLM_CONTRACT_ID":    "CPOOLXLM",
    "ESCROW_USDC_CONTRACT_ID": "CESCROWUSDC",
    "ESCROW_XLM_CONTRACT_ID":  "CESCROWXLM",
    "USDC_ISSUER":             "CUSDCISSUER",
}


STUB_STELLAR = r"""#!/usr/bin/env bash
# Deterministic fake `stellar` CLI for the invariant-report tests.
# Reads the invoked contract id and the trailing `--` method name and prints
# a canned response that satisfies the cross-contract invariants the script
# checks for.
id=""
method=""
asset=""
while [ $# -gt 0 ]; do
    case "$1" in
        --id)      id="$2"; shift 2;;
        --)        shift; method="$1"; shift; continue;;
        --asset)   asset="$2"; shift 2;;
        *) shift;;
    esac
done

case "$method" in
    get_escrow_contract)
        if [ "$id" = "CPOOLUSDC" ]; then echo '"CESCROWUSDC"'
        else echo '"CESCROWUSDC"'; fi ;;
    get_invoice_contract)        echo '"CINVOICE"' ;;
    get_agent_registry_contract) echo '"CREGISTRY"' ;;
    get_funding_asset)           echo '"CUSDCISSUER"' ;;
    is_supported_asset)
        if [ "$asset" = "CUSDCISSUER" ]; then echo 'true'
        else echo 'false'; fi ;;
    get_pool_stats)
        # Minimal JSON payload with the counters the script parses.
        echo '{"total_deposits":"1000","total_funded":"250","active_invoices":0}' ;;
    get_locked)
        echo '{"amount":"0","issuer":null}' ;;
    *)
        echo "unknown method: $method" >&2
        exit 1 ;;
esac
"""


def make_stub_tree() -> Path:
    tmp = Path(tempfile.mkdtemp(prefix="invariant-report-test-"))
    scripts_dir = tmp / "scripts"
    scripts_dir.mkdir()
    # The real script sources load_env.sh from the same directory.
    shutil.copy(REPO_ROOT / "scripts" / "load_env.sh", scripts_dir / "load_env.sh")
    shutil.copy(SCRIPT, scripts_dir / "invariant_report.sh")
    (scripts_dir / "invariant_report.sh").chmod(0o755)
    # Fake `stellar` that never touches the network.
    stub_stellar = tmp / "bin" / "stellar"
    stub_stellar.parent.mkdir()
    stub_stellar.write_text(STUB_STELLAR)
    stub_stellar.chmod(0o755)
    # .env with every required var
    env_lines = "\n".join(f"{k}={v}" for k, v in STUB_ENV.items()) + "\n"
    (tmp / ".env").write_text(env_lines)
    return tmp


def run_script(tmp: Path, *extra_args: str, env_overrides: dict | None = None) -> subprocess.CompletedProcess:
    env = os.environ.copy()
    env["PATH"] = f"{tmp / 'bin'}{os.pathsep}{env['PATH']}"
    if env_overrides:
        env.update(env_overrides)
    return subprocess.run(
        ["bash", str(tmp / "scripts" / "invariant_report.sh"), *extra_args],
        cwd=tmp,
        env=env,
        capture_output=True,
        text=True,
    )


class InvariantReportTests(unittest.TestCase):
    def setUp(self) -> None:
        if os.name == "nt":
            # The script is bash; on Windows CI uses ubuntu-24.04 so this is fine,
            # but we skip local Windows developer runs where WSL bash may not have
            # the fake-stellar PATH visible.
            if shutil.which("bash") is None:
                self.skipTest("bash not available on this platform")
        self.tmp = make_stub_tree()

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_successful_path_exits_zero(self) -> None:
        result = run_script(self.tmp)
        self.assertEqual(
            result.returncode, 0,
            f"expected exit 0, got {result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}",
        )
        self.assertIn("All invariants hold", result.stdout)
        self.assertNotIn("[FAIL]", result.stdout)

    def test_missing_env_var_exits_two(self) -> None:
        # Blank the USDC issuer in .env and confirm the script refuses to run.
        env_path = self.tmp / ".env"
        env_path.write_text(env_path.read_text().replace("CUSDCISSUER", ""))
        result = run_script(self.tmp)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("USDC_ISSUER", result.stderr)

    def test_missing_env_file_exits_two(self) -> None:
        (self.tmp / ".env").unlink()
        result = run_script(self.tmp)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(".env", result.stderr)

    def test_idempotent_two_consecutive_runs_match(self) -> None:
        a = run_script(self.tmp)
        b = run_script(self.tmp)
        self.assertEqual(a.returncode, 0)
        self.assertEqual(b.returncode, 0)
        # The report is a function of chain state; back-to-back runs against
        # the same fake chain must produce byte-identical summaries.
        def summary(text: str) -> str:
            return "\n".join(line for line in text.splitlines() if "/" in line and "checks passed" in line)
        self.assertEqual(summary(a.stdout), summary(b.stdout))

    def test_unknown_flag_exits_two(self) -> None:
        result = run_script(self.tmp, "--not-a-flag")
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
