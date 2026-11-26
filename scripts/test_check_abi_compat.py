"""Tests for scripts/check_abi_compat.py (issue #1206)."""
from __future__ import annotations

import json
import os
import shutil
import stat
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS_DIR = Path(__file__).resolve().parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

import check_abi_compat as abi
from soroban_spec import (
    CONTRACTS,
    ContractSpec,
    FnParam,
    FnSpec,
    StructField,
    StructSpec,
    TypeRef,
    encode_wasm_with_spec,
)

VALID_CONTRACT_IDS = {
    "registry": "CABGWVIZFF62FG67ZGFEP67NEEY4WYTMFURDMFTKKNRDAFPKPOJDTN4C",
    "invoice": "CA4O3MR7LWHRSUDBNU6FY6UDFFYBN7TGBZXBDZB4OYYXFYXIFJ6RJF6B",
    "escrow": "CAJWGUKDTTC3SKN4RAAY72J4DVIIYSCFHX6GIMNTT22ABMISJK4GBCEH",
    "pool": "CAKEWH7SJCXGV2MH2WZYIX3QDPTSSBQFXYVYBOWAGLNBBZMPLE2US6CS",
    "pool_factory": "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHQCYSC",
}


def _base_spec(extra_fn: bool = False, arg_type: str = "u128", ret_type: str = "bool") -> ContractSpec:
    fns = [
        FnSpec(
            name="ping",
            inputs=(FnParam("amount", TypeRef("primitive", arg_type)),),
            output=TypeRef("primitive", ret_type),
        ),
        FnSpec(
            name="get_item",
            inputs=(),
            output=TypeRef("udt", "Item"),
        ),
    ]
    if extra_fn:
        fns.append(
            FnSpec(
                name="new_view",
                inputs=(),
                output=TypeRef("primitive", "u32"),
            )
        )
    return ContractSpec(
        functions=tuple(fns),
        structs=(
            StructSpec(
                name="Item",
                fields=(StructField("value", TypeRef("primitive", "u128")),),
            ),
        ),
        unions=(),
        enums=(),
        error_enums=(),
    )


class CheckAbiCompatTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="abi-compat-test-"))
        self.local_dir = self.tmp / "local"
        self.remote_dir = self.tmp / "remote"
        self.local_dir.mkdir()
        self.remote_dir.mkdir()

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _write_pair(self, deployed: ContractSpec, local: ContractSpec) -> None:
        for _, _, wasm_name, _ in CONTRACTS:
            (self.remote_dir / wasm_name).write_bytes(encode_wasm_with_spec(deployed))
            (self.local_dir / wasm_name).write_bytes(encode_wasm_with_spec(local))

    def test_identical_and_additive_changes_exit_zero(self) -> None:
        self._write_pair(_base_spec(), _base_spec(extra_fn=True))
        rc = abi.main([
            "--wasm-dir", str(self.local_dir),
            "--remote-wasm-dir", str(self.remote_dir),
            "--json",
        ])
        self.assertEqual(rc, 0)

    def test_incompatible_arg_return_and_removed_fn_exit_one(self) -> None:
        deployed = _base_spec(extra_fn=True, arg_type="u128", ret_type="bool")
        local = _base_spec(extra_fn=False, arg_type="u64", ret_type="u32")
        self._write_pair(deployed, local)
        rc = abi.main([
            "--wasm-dir", str(self.local_dir),
            "--remote-wasm-dir", str(self.remote_dir),
        ])
        self.assertEqual(rc, 1)

    def test_incompatible_struct_layout_change_exits_one(self) -> None:
        deployed = _base_spec()
        local = ContractSpec(
            functions=deployed.functions,
            structs=(
                StructSpec(
                    name="Item",
                    fields=(
                        StructField("value", TypeRef("primitive", "u128")),
                        StructField("extra", TypeRef("primitive", "u32")),
                    ),
                ),
            ),
            unions=(),
            enums=(),
            error_enums=(),
        )
        self._write_pair(deployed, local)
        rc = abi.main([
            "--wasm-dir", str(self.local_dir),
            "--remote-wasm-dir", str(self.remote_dir),
        ])
        self.assertEqual(rc, 1)

    def test_invalid_contract_address_or_missing_wasm_exits_two(self) -> None:
        self._write_pair(_base_spec(), _base_spec())
        rc_bad_id = abi.main([
            "--wasm-dir", str(self.local_dir),
            "--contract", "registry=NOT_A_VALID_CONTRACT_ID",
        ])
        self.assertEqual(rc_bad_id, 2)

        rc_missing_wasm = abi.main([
            "--wasm-dir", str(self.tmp / "does-not-exist"),
            "--remote-wasm-dir", str(self.remote_dir),
        ])
        self.assertEqual(rc_missing_wasm, 2)

    def test_idempotent_report_and_summary_file_generation(self) -> None:
        self._write_pair(_base_spec(extra_fn=True), _base_spec(extra_fn=False))
        summary_a = self.tmp / "a.md"
        summary_b = self.tmp / "b.md"
        rc_a = abi.main([
            "--wasm-dir", str(self.local_dir),
            "--remote-wasm-dir", str(self.remote_dir),
            "--summary-file", str(summary_a),
            "--report-only",
        ])
        rc_b = abi.main([
            "--wasm-dir", str(self.local_dir),
            "--remote-wasm-dir", str(self.remote_dir),
            "--summary-file", str(summary_b),
            "--report-only",
        ])
        self.assertEqual(rc_a, 0)
        self.assertEqual(rc_b, 0)
        self.assertEqual(summary_a.read_text(encoding="utf-8"), summary_b.read_text(encoding="utf-8"))
        self.assertIn("removed_function", summary_a.read_text(encoding="utf-8"))

    def test_stubbed_stellar_cli_fetch_succeeds(self) -> None:
        self._write_pair(_base_spec(), _base_spec())
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        sample_wasm = (self.remote_dir / "trusttrove_registry.wasm").read_bytes()

        if os.name == "nt":
            stub = bin_dir / "stellar.cmd"
            py_exe = sys.executable
            helper = self.tmp / "fake_stellar.py"
            helper.write_text(
                f"import sys; from pathlib import Path\n"
                f"out = sys.argv[sys.argv.index('--out-file') + 1]\n"
                f"Path(out).write_bytes({sample_wasm!r})\n",
                encoding="utf-8",
            )
            stub.write_text(f'@echo off\n"{py_exe}" "{helper}" %*\n', encoding="utf-8")
        else:
            stub = bin_dir / "stellar"
            helper = self.tmp / "fake_stellar.py"
            helper.write_text(
                f"import sys; from pathlib import Path\n"
                f"out = sys.argv[sys.argv.index('--out-file') + 1]\n"
                f"Path(out).write_bytes({sample_wasm!r})\n",
                encoding="utf-8",
            )
            stub.write_text(f'#!/usr/bin/env bash\n"{sys.executable}" "{helper}" "$@"\n', encoding="utf-8")
            stub.chmod(stub.stat().st_mode | stat.S_IEXEC)

        old_path = os.environ.get("PATH", "")
        try:
            os.environ["PATH"] = f"{bin_dir}{os.pathsep}{old_path}"
            rc = abi.main([
                "--wasm-dir", str(self.local_dir),
                "--mode", "stellar",
                "--env-file", str(self.tmp / "no.env"),
                "--readme", str(self.tmp / "no.md"),
                "--contract", f"registry={VALID_CONTRACT_IDS['registry']}",
            ])
            self.assertEqual(rc, 0)
        finally:
            os.environ["PATH"] = old_path


if __name__ == "__main__":
    unittest.main()
