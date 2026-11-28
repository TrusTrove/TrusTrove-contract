"""Tests for scripts/check_deployed_hashes.py (issue #1299)."""
from __future__ import annotations

import hashlib
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPTS_DIR = Path(__file__).resolve().parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

import check_deployed_hashes as cdh
from soroban_spec import CONTRACTS

REGISTRY_ID = "CABGWVIZFF62FG67ZGFEP67NEEY4WYTMFURDMFTKKNRDAFPKPOJDTN4C"
INVOICE_ID = "CA4O3MR7LWHRSUDBNU6FY6UDFFYBN7TGBZXBDZB4OYYXFYXIFJ6RJF6B"
ESCROW_ID = "CAJWGUKDTTC3SKN4RAAY72J4DVIIYSCFHX6GIMNTT22ABMISJK4GBCEH"
POOL_ID = "CAKEWH7SJCXGV2MH2WZYIX3QDPTSSBQFXYVYBOWAGLNBBZMPLE2US6CS"


class CheckDeployedHashesTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="deployed-hashes-test-"))
        self.wasm_dir = self.tmp / "wasm"
        self.wasm_dir.mkdir()
        self.hashes: dict[str, str] = {}
        for key, _, wasm_name, _ in CONTRACTS:
            data = f"wasm-for-{key}".encode()
            (self.wasm_dir / wasm_name).write_bytes(data)
            self.hashes[key] = hashlib.sha256(data).hexdigest()

        self.manifest_path = self.tmp / "release-manifest.json"
        self.manifest = {
            "version": "0.1.0",
            "network": "testnet",
            "network_passphrase": "Test SDF Network ; September 2015",
            "contracts": {},
            "assets": {},
            "wasm_hashes": dict(self.hashes),
        }
        self.manifest_path.write_text(json.dumps(self.manifest), encoding="utf-8")

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _fake_fetch(self, contract_id: str, rpc_url: str):
        return b"", {
            REGISTRY_ID: self.hashes["registry"],
            INVOICE_ID: self.hashes["invoice"],
            ESCROW_ID: self.hashes["escrow"],
            POOL_ID: self.hashes["pool"],
        }[contract_id]

    def test_local_wasm_hashes_match_deployed_code(self) -> None:
        with mock.patch.object(cdh, "fetch_contract_wasm_rpc", side_effect=self._fake_fetch):
            rc = cdh.main(
                [
                    "--wasm-dir", str(self.wasm_dir),
                    "--contract", f"registry={REGISTRY_ID}",
                    "--contract", f"invoice={INVOICE_ID}",
                    "--contract", f"escrow_usdc={ESCROW_ID}",
                    "--contract", f"pool_usdc={POOL_ID}",
                    "--rpc-url", "https://example.invalid",
                ]
            )
        self.assertEqual(rc, 0)

    def test_manifest_hashes_match_deployed_code(self) -> None:
        with mock.patch.object(cdh, "fetch_contract_wasm_rpc", side_effect=self._fake_fetch):
            rc = cdh.main(
                [
                    "--manifest", str(self.manifest_path),
                    "--contract", f"registry={REGISTRY_ID}",
                    "--rpc-url", "https://example.invalid",
                ]
            )
        self.assertEqual(rc, 0)

    def test_onchain_mismatch_fails_before_wiring(self) -> None:
        def stale_fetch(contract_id: str, rpc_url: str):
            return b"", "0" * 64

        with mock.patch.object(cdh, "fetch_contract_wasm_rpc", side_effect=stale_fetch):
            rc = cdh.main(
                [
                    "--wasm-dir", str(self.wasm_dir),
                    "--contract", f"pool_usdc={POOL_ID}",
                    "--rpc-url", "https://example.invalid",
                ]
            )
        self.assertEqual(rc, 1)

    def test_manifest_mismatch_fails(self) -> None:
        stale = dict(self.manifest)
        stale["wasm_hashes"] = dict(self.hashes, escrow="f" * 64)
        self.manifest_path.write_text(json.dumps(stale), encoding="utf-8")
        with mock.patch.object(cdh, "fetch_contract_wasm_rpc", side_effect=self._fake_fetch):
            rc = cdh.main(
                [
                    "--manifest", str(self.manifest_path),
                    "--contract", f"escrow_usdc={ESCROW_ID}",
                    "--rpc-url", "https://example.invalid",
                ]
            )
        self.assertEqual(rc, 1)

    def test_unreadable_contract_fails(self) -> None:
        def broken_fetch(contract_id: str, rpc_url: str):
            raise RuntimeError("contract instance not found on-chain")

        with mock.patch.object(cdh, "fetch_contract_wasm_rpc", side_effect=broken_fetch):
            rc = cdh.main(
                [
                    "--wasm-dir", str(self.wasm_dir),
                    "--contract", f"registry={REGISTRY_ID}",
                    "--rpc-url", "https://example.invalid",
                ]
            )
        self.assertEqual(rc, 1)

    def test_missing_artifact_and_invalid_arguments_exit_two(self) -> None:
        self.assertEqual(
            cdh.main(["--wasm-dir", str(self.tmp / "absent"), "--contract", f"registry={REGISTRY_ID}"]),
            2,
        )
        self.assertEqual(cdh.main(["--wasm-dir", str(self.wasm_dir), "--contract", "badarg"]), 2)
        self.assertEqual(
            cdh.main(["--wasm-dir", str(self.wasm_dir), "--contract", f"unknown={REGISTRY_ID}"]),
            2,
        )
        self.assertEqual(cdh.main(["--manifest", str(self.tmp / "absent.json")]), 2)


if __name__ == "__main__":
    unittest.main()
