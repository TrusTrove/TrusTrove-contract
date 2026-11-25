"""Tests for scripts/verify_release.py (issue #1212)."""
from __future__ import annotations

import base64
import hashlib
import json
import os
import shutil
import stat
import struct
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPTS_DIR = Path(__file__).resolve().parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

import verify_release as vr
from soroban_spec import CONTRACTS, ContractSpec, TypeRef, FnSpec, crc16_xmodem, encode_wasm_with_spec


def _make_contract_id(fill: int) -> str:
    body = bytes([2 << 3]) + bytes([fill]) * 32
    return base64.b32encode(body + struct.pack("<H", crc16_xmodem(body))).decode("ascii")


ADDRS = {
    "registry": "CABGWVIZFF62FG67ZGFEP67NEEY4WYTMFURDMFTKKNRDAFPKPOJDTN4C",
    "invoice": "CA4O3MR7LWHRSUDBNU6FY6UDFFYBN7TGBZXBDZB4OYYXFYXIFJ6RJF6B",
    "escrow_usdc": "CAJWGUKDTTC3SKN4RAAY72J4DVIIYSCFHX6GIMNTT22ABMISJK4GBCEH",
    "pool_usdc": "CAKEWH7SJCXGV2MH2WZYIX3QDPTSSBQFXYVYBOWAGLNBBZMPLE2US6CS",
    "pool_factory": _make_contract_id(0x42),
    "usdc": "GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5",
}


class VerifyReleaseTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="verify-release-test-"))
        self.wasm_dir = self.tmp / "wasm"
        self.wasm_dir.mkdir()
        self.wasm_hashes: dict[str, str] = {}

        for key, _, wasm_name, _ in CONTRACTS:
            spec = ContractSpec(
                functions=(FnSpec("get_admin", (), TypeRef("primitive", "Address")),),
                structs=(),
                unions=(),
                enums=(),
                error_enums=(),
            )
            data = encode_wasm_with_spec(spec)
            (self.wasm_dir / wasm_name).write_bytes(data)
            self.wasm_hashes[key] = hashlib.sha256(data).hexdigest()

        self.manifest_path = self.tmp / "release-manifest.json"
        self.env_path = self.tmp / ".env"
        self.env_deployed_path = self.tmp / ".env.deployed"
        self.deployments_path = self.tmp / "deployments.json"

        self.manifest = {
            "version": "0.1.0",
            "network": "testnet",
            "network_passphrase": "Test SDF Network ; September 2015",
            "rpc_url": "https://soroban-testnet.stellar.org",
            "contracts": {
                "registry": ADDRS["registry"],
                "invoice": ADDRS["invoice"],
                "escrow_usdc": ADDRS["escrow_usdc"],
                "pool_usdc": ADDRS["pool_usdc"],
                "pool_factory": ADDRS["pool_factory"],
            },
            "assets": {"usdc": ADDRS["usdc"]},
            "wasm_hashes": dict(self.wasm_hashes),
        }
        self.manifest_path.write_text(json.dumps(self.manifest, indent=2), encoding="utf-8")

        self.env_path.write_text(
            "\n".join(
                [
                    "STELLAR_NETWORK=testnet",
                    'STELLAR_NETWORK_PASSPHRASE="Test SDF Network ; September 2015"',
                    "STELLAR_RPC_URL=https://soroban-testnet.stellar.org",
                    "DEPLOYER_ACCOUNT=deployer",
                    f"USDC_ISSUER={ADDRS['usdc']}",
                    f"REGISTRY_CONTRACT_ID={ADDRS['registry']}",
                    f"INVOICE_CONTRACT_ID={ADDRS['invoice']}",
                    f"ESCROW_USDC_CONTRACT_ID={ADDRS['escrow_usdc']}",
                    f"POOL_USDC_CONTRACT_ID={ADDRS['pool_usdc']}",
                    f"POOL_FACTORY_CONTRACT_ID={ADDRS['pool_factory']}",
                ]
            )
            + "\n",
            encoding="utf-8",
        )
        self.env_deployed_path.write_text(
            "\n".join(
                [
                    f"NEXT_PUBLIC_REGISTRY_CONTRACT_ID={ADDRS['registry']}",
                    f"NEXT_PUBLIC_INVOICE_CONTRACT_ID={ADDRS['invoice']}",
                    f"NEXT_PUBLIC_ESCROW_USDC_CONTRACT_ID={ADDRS['escrow_usdc']}",
                    f"NEXT_PUBLIC_POOL_USDC_CONTRACT_ID={ADDRS['pool_usdc']}",
                ]
            )
            + "\n",
            encoding="utf-8",
        )
        self.deployments_path.write_text(
            json.dumps(
                {
                    "registry": ADDRS["registry"],
                    "invoice": ADDRS["invoice"],
                    "escrow_usdc": ADDRS["escrow_usdc"],
                    "pool_usdc": ADDRS["pool_usdc"],
                }
            ),
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_successful_manifest_and_cross_checks_exit_zero(self) -> None:
        rc = vr.main([
            "--manifest", str(self.manifest_path),
            "--wasm-dir", str(self.wasm_dir),
            "--network", "testnet",
            "--env-file", str(self.env_path),
            "--env-deployed", str(self.env_deployed_path),
            "--deployments-json", str(self.deployments_path),
        ])
        self.assertEqual(rc, 0)

    def test_wasm_hash_mismatch_exits_one(self) -> None:
        bad = dict(self.manifest)
        bad["wasm_hashes"] = dict(self.wasm_hashes, pool="0" * 64)
        self.manifest_path.write_text(json.dumps(bad), encoding="utf-8")
        rc = vr.main(["--manifest", str(self.manifest_path), "--wasm-dir", str(self.wasm_dir)])
        self.assertEqual(rc, 1)

    def test_invalid_strkey_duplicate_address_and_network_mismatch_exit_one(self) -> None:
        bad = dict(self.manifest)
        bad["network"] = "mainnet"
        bad["contracts"] = dict(self.manifest["contracts"], invoice=ADDRS["registry"], escrow_usdc="CBAD")
        self.manifest_path.write_text(json.dumps(bad), encoding="utf-8")
        rc = vr.main([
            "--manifest", str(self.manifest_path),
            "--wasm-dir", str(self.wasm_dir),
            "--network", "testnet",
        ])
        self.assertEqual(rc, 1)

    def test_missing_or_malformed_manifest_exits_two(self) -> None:
        self.assertEqual(vr.main(["--manifest", str(self.tmp / "absent.json")]), 2)
        bad_json = self.tmp / "bad.json"
        bad_json.write_text("{not-json", encoding="utf-8")
        self.assertEqual(vr.main(["--manifest", str(bad_json)]), 2)

    def test_generate_manifest_roundtrip_and_idempotency(self) -> None:
        gen_path = self.tmp / "generated.json"
        self.assertEqual(
            vr.main([
                "--generate-manifest", str(gen_path),
                "--wasm-dir", str(self.wasm_dir),
                "--network", "testnet",
                "--env-file", str(self.env_path),
                "--deployments-json", str(self.deployments_path),
            ]),
            0,
        )
        res_a = vr.verify_manifest(json.loads(gen_path.read_text(encoding="utf-8")), self.wasm_dir, "testnet")
        res_b = vr.verify_manifest(json.loads(gen_path.read_text(encoding="utf-8")), self.wasm_dir, "testnet")
        self.assertEqual(res_a, res_b)
        self.assertTrue(all(ok for _, ok, _ in res_a))

    def test_check_wasm_only_succeeds(self) -> None:
        self.assertEqual(vr.main(["--check-wasm-only", "--wasm-dir", str(self.wasm_dir), "--json"]), 0)

    def test_onchain_wiring_and_hash_check_with_stubs(self) -> None:
        def fake_fetch(cid: str, rpc_url: str, timeout: int = 20):
            reverse = {
                ADDRS["registry"]: "registry",
                ADDRS["invoice"]: "invoice",
                ADDRS["escrow_usdc"]: "escrow",
                ADDRS["pool_usdc"]: "pool",
                ADDRS["pool_factory"]: "pool_factory",
            }
            return b"", self.wasm_hashes[reverse[cid]]

        def fake_invoke(cli: str, cid: str, source: str, network: str, method: str) -> str:
            responses = {
                (ADDRS["pool_usdc"], "get_invoice_contract"): ADDRS["invoice"],
                (ADDRS["pool_usdc"], "get_escrow_contract"): ADDRS["escrow_usdc"],
                (ADDRS["invoice"], "get_pool_contract"): ADDRS["pool_usdc"],
                (ADDRS["invoice"], "get_escrow_contract"): ADDRS["escrow_usdc"],
                (ADDRS["escrow_usdc"], "get_pool_contract"): ADDRS["pool_usdc"],
                (ADDRS["escrow_usdc"], "get_invoice_contract"): ADDRS["invoice"],
            }
            return responses[(cid, method)]

        with mock.patch.object(vr, "fetch_contract_wasm_rpc", side_effect=fake_fetch), \
             mock.patch.object(vr, "find_stellar_cli", return_value="stellar"), \
             mock.patch.object(vr, "_invoke_view", side_effect=fake_invoke):
            rc = vr.main([
                "--manifest", str(self.manifest_path),
                "--wasm-dir", str(self.wasm_dir),
                "--network", "testnet",
                "--check-onchain",
            ])
            self.assertEqual(rc, 0)


if __name__ == "__main__":
    unittest.main()
