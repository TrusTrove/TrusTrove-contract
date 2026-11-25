"""Tests for scripts/generate_ts_bindings.py (issue #1205)."""
from __future__ import annotations

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS_DIR = Path(__file__).resolve().parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

import generate_ts_bindings as gen
from soroban_spec import (
    CONTRACTS,
    ContractSpec,
    EnumCase,
    EnumSpec,
    FnParam,
    FnSpec,
    StructField,
    StructSpec,
    TypeRef,
    UnionCase,
    UnionSpec,
    encode_wasm_with_spec,
    parse_rust_contract_spec,
    parse_wasm_spec,
)


def _sample_spec() -> ContractSpec:
    return ContractSpec(
        functions=(
            FnSpec(
                name="__constructor",
                inputs=(FnParam("admin", TypeRef("primitive", "Address")),),
                output=TypeRef("primitive", "void"),
            ),
            FnSpec(
                name="submit",
                inputs=(
                    FnParam("id", TypeRef("bytes_n", length=32)),
                    FnParam("status", TypeRef("udt", "Status")),
                    FnParam(
                        "entries",
                        TypeRef(
                            "vec",
                            inner=TypeRef(
                                "tuple",
                                elements=(
                                    TypeRef("primitive", "Address"),
                                    TypeRef("primitive", "u128"),
                                ),
                            ),
                        ),
                    ),
                ),
                output=TypeRef("option", inner=TypeRef("udt", "Record")),
            ),
        ),
        structs=(
            StructSpec(
                name="Record",
                fields=(
                    StructField("amount", TypeRef("primitive", "u128")),
                    StructField(
                        "meta",
                        TypeRef("map", key=TypeRef("primitive", "String"), value=TypeRef("primitive", "String")),
                    ),
                ),
            ),
        ),
        unions=(
            UnionSpec(
                name="Status",
                cases=(
                    UnionCase("Active", ()),
                    UnionCase("Closed", ()),
                ),
            ),
            # Unreachable internal storage enum should be filtered out.
            UnionSpec(
                name="DataKey",
                cases=(UnionCase("Admin", ()),),
            ),
        ),
        enums=(),
        error_enums=(
            EnumSpec(
                name="RegistryError",
                cases=(EnumCase("AlreadyInitialized", 1), EnumCase("NotFound", 3)),
            ),
            # Shared workspace pause error should be filtered out.
            EnumSpec(
                name="PauseError",
                cases=(EnumCase("ContractPaused", 1),),
            ),
        ),
    )


class GenerateTsBindingsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="ts-bindings-test-"))
        self.wasm_dir = self.tmp / "wasm"
        self.out_dir = self.tmp / "out"
        self.wasm_dir.mkdir()

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _populate_synthetic_wasms(self) -> None:
        sample = _sample_spec()
        for key, pascal, wasm_name, _ in CONTRACTS:
            err_spec = EnumSpec(
                name=f"{pascal}Error",
                cases=(EnumCase("NotInitialized", 1),),
            )
            spec = ContractSpec(
                functions=sample.functions,
                structs=sample.structs,
                unions=sample.unions,
                enums=sample.enums,
                error_enums=(err_spec, sample.error_enums[1]),
            )
            (self.wasm_dir / wasm_name).write_bytes(encode_wasm_with_spec(spec))

    def test_successful_wasm_write_and_check_exits_zero(self) -> None:
        self._populate_synthetic_wasms()
        rc_write = gen.main(["--write", "--wasm-dir", str(self.wasm_dir), "--out-dir", str(self.out_dir)])
        self.assertEqual(rc_write, 0)
        rc_check = gen.main(["--check", "--wasm-dir", str(self.wasm_dir), "--out-dir", str(self.out_dir)])
        self.assertEqual(rc_check, 0)

        registry_ts = (self.out_dir / "registry.ts").read_text(encoding="utf-8")
        self.assertIn("export interface RegistryClient", registry_ts)
        self.assertIn("export enum RegistryError", registry_ts)
        self.assertIn("export interface Record", registry_ts)
        self.assertIn("export type Status =", registry_ts)
        self.assertIn("readonly [Address, u128]", registry_ts)
        self.assertIn("BytesN<32>", registry_ts)
        self.assertNotIn("DataKey", registry_ts)
        self.assertNotIn("PauseError", registry_ts)
        self.assertTrue((self.out_dir / "index.ts").is_file())

    def test_stale_binding_fails_check_with_exit_one(self) -> None:
        self._populate_synthetic_wasms()
        self.assertEqual(gen.main(["--write", "--wasm-dir", str(self.wasm_dir), "--out-dir", str(self.out_dir)]), 0)
        target = self.out_dir / "invoice.ts"
        target.write_text(target.read_text(encoding="utf-8") + "\n// drifted\n", encoding="utf-8")
        rc = gen.main(["--check", "--wasm-dir", str(self.wasm_dir), "--out-dir", str(self.out_dir)])
        self.assertEqual(rc, 1)

    def test_missing_or_corrupt_wasm_exits_two(self) -> None:
        rc_missing = gen.main(["--check", "--wasm-dir", str(self.wasm_dir), "--out-dir", str(self.out_dir)])
        self.assertEqual(rc_missing, 2)

        self._populate_synthetic_wasms()
        (self.wasm_dir / "trusttrove_registry.wasm").write_bytes(b"not-a-wasm")
        rc_corrupt = gen.main(["--check", "--wasm-dir", str(self.wasm_dir), "--out-dir", str(self.out_dir)])
        self.assertEqual(rc_corrupt, 2)

    def test_idempotent_consecutive_runs_and_wasm_roundtrip(self) -> None:
        self._populate_synthetic_wasms()
        self.assertEqual(gen.main(["--write", "--wasm-dir", str(self.wasm_dir), "--out-dir", str(self.out_dir)]), 0)
        first = {p.name: p.read_text(encoding="utf-8") for p in sorted(self.out_dir.glob("*.ts"))}
        self.assertEqual(gen.main(["--write", "--wasm-dir", str(self.wasm_dir), "--out-dir", str(self.out_dir)]), 0)
        second = {p.name: p.read_text(encoding="utf-8") for p in sorted(self.out_dir.glob("*.ts"))}
        self.assertEqual(first, second)

        # Verify repo Rust sources round-trip cleanly through WASM spec encoding
        for _, pascal, _, crate_rel in CONTRACTS:
            rust_spec = parse_rust_contract_spec(Path(crate_rel)).canonical(pascal)
            wasm_bytes = encode_wasm_with_spec(rust_spec)
            decoded = parse_wasm_spec(wasm_bytes).canonical(pascal)
            self.assertEqual(rust_spec, decoded)

    def test_committed_bindings_match_contract_sources(self) -> None:
        rc = gen.main(["--check", "--from-source"])
        self.assertEqual(rc, 0)


if __name__ == "__main__":
    unittest.main()
