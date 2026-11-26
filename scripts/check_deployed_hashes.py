#!/usr/bin/env python3
"""Preflight deployed-code hash check before reference-setting wiring (#1299).

Deployment scripts must not wire contract IDs that are running unexpected WASM.
This checker fetches each deployed contract's code hash from Soroban RPC and
compares it with the intended release:

* ``--manifest`` uses the ``wasm_hashes`` map of a release manifest produced by
  ``scripts/verify_release.py --generate-manifest``.
* ``--wasm-dir`` (the default when no manifest is supplied) hashes the freshly
  built local WASM artifacts that the deploy step just installed.

It performs no transaction and writes nothing. On any mismatch, missing
artifact, or unreadable on-chain contract it prints the failure and exits `1`,
so the caller can abort before the first reference-setting call.

Exit codes:
    0 — every deployed contract matches the intended release
    1 — a hash is missing or mismatched, or a contract could not be read
    2 — invalid CLI arguments or malformed manifest
"""
from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Callable

from soroban_spec import CONTRACTS, ROOT, fetch_contract_wasm_rpc

DEFAULT_WASM_DIR = ROOT / "target" / "wasm32v1-none" / "release"

DEFAULT_RPC_URLS: dict[str, str] = {
    "testnet": "https://soroban-testnet.stellar.org",
    "mainnet": "https://mainnet.sorobanrpc.com",
    "public": "https://mainnet.sorobanrpc.com",
    "futurenet": "https://rpc-futurenet.stellar.org",
}

# Deployment key -> key in the release manifest's `wasm_hashes` map. The XLM
# pair reuses the same escrow/pool WASM as the USDC pair.
HASH_KEY_BY_CONTRACT: dict[str, str] = {
    "registry": "registry",
    "invoice": "invoice",
    "escrow_usdc": "escrow",
    "pool_usdc": "pool",
    "escrow_xlm": "escrow",
    "pool_xlm": "pool",
    "pool_factory": "pool_factory",
}

WASM_FILE_BY_HASH_KEY: dict[str, str] = {key: wasm_name for key, _, wasm_name, _ in CONTRACTS}

FetchFn = Callable[[str, str], "tuple[bytes, str]"]


def _is_hex64(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(char in "0123456789abcdef" for char in value.lower())
    )


def expected_hashes_from_manifest(manifest: dict) -> tuple[dict[str, str], list[str]]:
    """Return ``{hash_key: sha256_hex}`` from a validated release manifest."""
    hashes = manifest.get("wasm_hashes")
    if not isinstance(hashes, dict):
        return {}, ["manifest.wasm_hashes must be an object"]
    expected: dict[str, str] = {}
    errors: list[str] = []
    for key in WASM_FILE_BY_HASH_KEY:
        value = hashes.get(key)
        if not _is_hex64(value):
            errors.append(f"manifest.wasm_hashes.{key} is not a 64-hex SHA-256: {value!r}")
        else:
            expected[key] = str(value).lower()
    return expected, errors


def expected_hashes_from_wasm_dir(wasm_dir: Path) -> tuple[dict[str, str], list[str]]:
    """Hash the local release artifacts named by the workspace manifest."""
    expected: dict[str, str] = {}
    errors: list[str] = []
    for key, wasm_name in WASM_FILE_BY_HASH_KEY.items():
        path = wasm_dir / wasm_name
        if not path.is_file():
            errors.append(f"missing local WASM artifact: {path}")
            continue
        expected[key] = hashlib.sha256(path.read_bytes()).hexdigest()
    return expected, errors


def parse_contract_args(values: list[str]) -> tuple[dict[str, str], list[str]]:
    """Parse repeated ``--contract KEY=CONTRACT_ID`` arguments."""
    contracts: dict[str, str] = {}
    errors: list[str] = []
    for value in values:
        key, separator, contract_id = value.partition("=")
        key, contract_id = key.strip(), contract_id.strip()
        if not separator or not key or not contract_id:
            errors.append(f"invalid --contract (expected KEY=CONTRACT_ID): {value!r}")
            continue
        if key not in HASH_KEY_BY_CONTRACT:
            errors.append(f"unknown contract key {key!r}; expected one of {', '.join(HASH_KEY_BY_CONTRACT)}")
            continue
        contracts[key] = contract_id
    return contracts, errors


def verify_deployed_hashes(
    expected: dict[str, str],
    contracts: dict[str, str],
    rpc_url: str,
    fetch: FetchFn | None = None,
) -> list[tuple[str, bool, str]]:
    """Compare each deployed contract's on-chain hash with ``expected``.

    Returns ``[(check_name, passed, detail)]``. No transaction is submitted.
    ``fetch`` is injectable for tests and defaults to the Soroban RPC reader.
    """
    fetch = fetch or fetch_contract_wasm_rpc
    results: list[tuple[str, bool, str]] = []
    for contract_key, contract_id in contracts.items():
        hash_key = HASH_KEY_BY_CONTRACT[contract_key]
        wanted = expected.get(hash_key)
        name = f"deployed_hash.{contract_key}"
        if wanted is None:
            results.append((name, False, f"no expected hash for manifest key {hash_key!r}"))
            continue
        try:
            _, onchain = fetch(contract_id, rpc_url)
        except Exception as exc:  # network/RPC/StrKey failures all block wiring
            results.append((name, False, f"could not read {contract_id}: {exc}"))
            continue
        actual = onchain.lower()
        if actual == wanted:
            results.append((name, True, f"sha256:{actual[:16]}.. matches {hash_key}"))
        else:
            results.append(
                (name, False, f"onchain sha256:{actual} != expected {hash_key} sha256:{wanted}")
            )
    return results


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--manifest", type=Path, help="release manifest JSON with wasm_hashes")
    source.add_argument("--wasm-dir", type=Path, help="local WASM directory to hash (default: build output)")
    parser.add_argument(
        "--contract",
        action="append",
        default=[],
        metavar="KEY=CONTRACT_ID",
        help="deployed contract to check; repeat for each contract",
    )
    parser.add_argument("--network", default="testnet", help="target network label (default: testnet)")
    parser.add_argument("--rpc-url", default=None, help="Soroban RPC URL override")
    parser.add_argument("--json", action="store_true", help="emit machine-readable results")
    args = parser.parse_args(argv)

    if args.manifest is not None:
        if not args.manifest.is_file():
            print(f"Error: manifest file not found: {args.manifest}", file=sys.stderr)
            return 2
        try:
            manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
            if not isinstance(manifest, dict):
                raise ValueError("manifest root must be a JSON object")
        except (OSError, ValueError, json.JSONDecodeError) as exc:
            print(f"Error reading manifest {args.manifest}: {exc}", file=sys.stderr)
            return 2
        expected, errors = expected_hashes_from_manifest(manifest)
    else:
        wasm_dir = args.wasm_dir or DEFAULT_WASM_DIR
        expected, errors = expected_hashes_from_wasm_dir(wasm_dir)

    contracts, contract_errors = parse_contract_args(args.contract)
    errors.extend(contract_errors)
    if not contracts and not errors:
        errors.append("no --contract KEY=CONTRACT_ID given")

    rpc_url = args.rpc_url or DEFAULT_RPC_URLS.get(args.network)
    if rpc_url is None:
        errors.append(f"unknown network {args.network!r}; pass --rpc-url")

    if errors:
        if args.json:
            print(json.dumps({"passed": 0, "failed": len(errors), "errors": errors}, indent=2))
        else:
            for error in errors:
                print(f"  [FAIL] {error}", file=sys.stderr)
        return 2

    results = verify_deployed_hashes(expected, contracts, rpc_url)
    failed = [name for name, ok, _ in results if not ok]

    if args.json:
        print(
            json.dumps(
                {
                    "passed": len(results) - len(failed),
                    "failed": len(failed),
                    "checks": [{"name": name, "ok": ok, "detail": detail} for name, ok, detail in results],
                },
                indent=2,
            )
        )
    else:
        print(f"Deployed code hash preflight ({len(results) - len(failed)}/{len(results)} passed)")
        for name, ok, detail in results:
            tag = "PASS" if ok else "FAIL"
            print(f"  [{tag}] {name}: {detail}")

    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
