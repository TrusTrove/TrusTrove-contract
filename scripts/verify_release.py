#!/usr/bin/env python3
"""Read-only release checklist verifier for TrusTrove Soroban contracts (#1212).

Validates:
1. Release manifest completeness (version, network, contracts, assets, wasm_hashes)
2. Local compiled WASM artifact integrity and SHA-256 hashes
3. Target network & passphrase consistency (including .env cross-checks)
4. All configured contract & asset StrKey references (and optional on-chain wiring)

Exit codes:
    0 — All release checklist checks passed
    1 — One or more verification checks failed
    2 — Missing input file, malformed JSON, or invalid CLI arguments
"""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path

from soroban_spec import (
    CONTRACTS,
    ROOT,
    WASM_MAGIC,
    extract_contract_spec_bytes,
    fetch_contract_wasm_rpc,
    find_stellar_cli,
    load_env_file,
    validate_strkey,
)

DEFAULT_WASM_DIR = ROOT / "target" / "wasm32v1-none" / "release"

NETWORK_PASSPHRASES: dict[str, str] = {
    "testnet": "Test SDF Network ; September 2015",
    "mainnet": "Public Global Stellar Network ; September 2015",
    "public": "Public Global Stellar Network ; September 2015",
    "futurenet": "Test SDF Future Network ; October 2022",
}

DEFAULT_RPC_URLS: dict[str, str] = {
    "testnet": "https://soroban-testnet.stellar.org",
    "mainnet": "https://mainnet.sorobanrpc.com",
    "public": "https://mainnet.sorobanrpc.com",
    "futurenet": "https://rpc-futurenet.stellar.org",
}

REQUIRED_CONTRACTS = ("registry", "invoice", "escrow_usdc", "pool_usdc")
OPTIONAL_CONTRACTS = ("pool_factory", "escrow_xlm", "pool_xlm", "agent_registry")

ENV_CONTRACT_KEYS: dict[str, str] = {
    "registry": "REGISTRY_CONTRACT_ID",
    "invoice": "INVOICE_CONTRACT_ID",
    "escrow_usdc": "ESCROW_USDC_CONTRACT_ID",
    "pool_usdc": "POOL_USDC_CONTRACT_ID",
    "pool_factory": "POOL_FACTORY_CONTRACT_ID",
    "escrow_xlm": "ESCROW_XLM_CONTRACT_ID",
    "pool_xlm": "POOL_XLM_CONTRACT_ID",
    "agent_registry": "AGENT_REGISTRY_CONTRACT",
}

ENV_DEPLOYED_KEYS: dict[str, str] = {
    "registry": "NEXT_PUBLIC_REGISTRY_CONTRACT_ID",
    "invoice": "NEXT_PUBLIC_INVOICE_CONTRACT_ID",
    "escrow_usdc": "NEXT_PUBLIC_ESCROW_USDC_CONTRACT_ID",
    "pool_usdc": "NEXT_PUBLIC_POOL_USDC_CONTRACT_ID",
    "escrow_xlm": "NEXT_PUBLIC_ESCROW_XLM_CONTRACT_ID",
    "pool_xlm": "NEXT_PUBLIC_POOL_XLM_CONTRACT_ID",
}


def compute_wasm_hashes(wasm_dir: Path) -> tuple[dict[str, str], list[str]]:
    """Verify each compiled WASM binary and return {contract_key: sha256_hex}."""
    hashes: dict[str, str] = {}
    errors: list[str] = []
    for key, _, wasm_name, _ in CONTRACTS:
        path = wasm_dir / wasm_name
        if not path.is_file():
            errors.append(f"missing WASM artifact: {path}")
            continue
        raw = path.read_bytes()
        if len(raw) <= len(WASM_MAGIC) or raw[: len(WASM_MAGIC)] != WASM_MAGIC:
            errors.append(f"invalid WASM binary header: {path}")
            continue
        try:
            extract_contract_spec_bytes(raw)
        except ValueError as exc:
            errors.append(f"invalid Soroban spec in {path.name}: {exc}")
            continue
        hashes[key] = hashlib.sha256(raw).hexdigest()
    return hashes, errors


def _is_hex64(value: object) -> bool:
    return isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value.lower())


def _check_url_network(url: str, network: str) -> str | None:
    lowered = url.lower()
    if network in {"mainnet", "public"} and ("testnet" in lowered or "futurenet" in lowered):
        return f"URL '{url}' points to a non-mainnet network while target network is '{network}'"
    if network == "testnet" and ("mainnet" in lowered or "futurenet" in lowered):
        return f"URL '{url}' does not match target network 'testnet'"
    if network == "futurenet" and ("testnet" in lowered or "mainnet" in lowered):
        return f"URL '{url}' does not match target network 'futurenet'"
    return None


def _invoke_view(stellar_bin: str, contract_id: str, source: str, network: str, method: str) -> str:
    proc = subprocess.run(
        [
            stellar_bin,
            "contract",
            "invoke",
            "--id",
            contract_id,
            "--source",
            source,
            "--network",
            network,
            "--send=no",
            "--",
            method,
        ],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip() or f"exit {proc.returncode}")
    return proc.stdout.strip().strip('"')


def verify_manifest(
    manifest: dict,
    wasm_dir: Path,
    target_network: str | None = None,
    env_file: Path | None = None,
    env_deployed: Path | None = None,
    deployments_json: Path | None = None,
    check_onchain: bool = False,
) -> list[tuple[str, bool, str]]:
    """Run all release checklist validations and return [(name, passed, detail)]."""
    results: list[tuple[str, bool, str]] = []

    def record(name: str, ok: bool, detail: str) -> None:
        results.append((name, ok, detail))

    # 1. Manifest completeness
    required_top = ("version", "network", "network_passphrase", "contracts", "assets", "wasm_hashes")
    missing_top = [k for k in required_top if k not in manifest]
    if missing_top:
        record("manifest.required_fields", False, f"missing top-level keys: {', '.join(missing_top)}")
        return results
    version_ok = isinstance(manifest["version"], str) and bool(manifest["version"].strip())
    record("manifest.version", version_ok, f"version={manifest['version']!r}")

    # 2. Target network validation
    m_net = str(manifest.get("network", "")).strip()
    if m_net not in NETWORK_PASSPHRASES:
        record("network.valid", False, f"unsupported network {m_net!r}")
    elif target_network and m_net != target_network:
        record("network.target_match", False, f"manifest network={m_net!r} != --network={target_network!r}")
    else:
        record("network.target_match", True, f"network={m_net}")

    expected_passphrase = NETWORK_PASSPHRASES.get(m_net)
    actual_passphrase = str(manifest.get("network_passphrase", "")).strip()
    if expected_passphrase and actual_passphrase == expected_passphrase:
        record("network.passphrase", True, actual_passphrase)
    else:
        record(
            "network.passphrase",
            False,
            f"expected {expected_passphrase!r} for {m_net!r}, got {actual_passphrase!r}",
        )

    rpc_url = str(manifest.get("rpc_url", "")).strip()
    if rpc_url:
        url_err = _check_url_network(rpc_url, m_net)
        record("network.rpc_url", url_err is None, url_err or rpc_url)

    # 3. WASM artifact existence & SHA-256 hashes
    computed_hashes, wasm_errors = compute_wasm_hashes(wasm_dir)
    manifest_hashes = manifest.get("wasm_hashes")
    if wasm_errors:
        record("wasm.artifacts", False, "; ".join(wasm_errors))
    else:
        record("wasm.artifacts", True, f"{len(computed_hashes)} valid Soroban WASM binary(ies)")

    if not isinstance(manifest_hashes, dict):
        record("wasm.hashes_map", False, "manifest.wasm_hashes must be an object")
    else:
        for key, _, _, _ in CONTRACTS:
            expected_hash = manifest_hashes.get(key)
            actual_hash = computed_hashes.get(key)
            if not _is_hex64(expected_hash):
                record(f"wasm.hash.{key}", False, f"invalid or missing 64-hex SHA-256 in manifest: {expected_hash!r}")
            elif actual_hash is None:
                record(f"wasm.hash.{key}", False, "local WASM missing/invalid")
            elif expected_hash.lower() != actual_hash.lower():
                record(
                    f"wasm.hash.{key}",
                    False,
                    f"hash mismatch: manifest={expected_hash.lower()} local={actual_hash.lower()}",
                )
            else:
                record(f"wasm.hash.{key}", True, f"sha256:{actual_hash[:16]}..")

    # 4. Contract & asset StrKey validation and uniqueness
    contracts = manifest.get("contracts")
    seen_addresses: dict[str, str] = {}
    if not isinstance(contracts, dict):
        record("contracts.map", False, "manifest.contracts must be an object")
    else:
        for ckey in REQUIRED_CONTRACTS:
            cid = str(contracts.get(ckey, "")).strip()
            ok, err = validate_strkey(cid, expected_prefix="C")
            if not ok:
                record(f"contracts.{ckey}", False, f"invalid/missing contract ID: {err}")
            elif cid in seen_addresses:
                record(
                    f"contracts.{ckey}",
                    False,
                    f"duplicate address {cid} already assigned to {seen_addresses[cid]}",
                )
            else:
                seen_addresses[cid] = ckey
                record(f"contracts.{ckey}", True, cid)

        for ckey in OPTIONAL_CONTRACTS:
            cid = str(contracts.get(ckey, "") or "").strip()
            if not cid:
                continue
            ok, err = validate_strkey(cid, expected_prefix="C")
            if not ok:
                record(f"contracts.{ckey}", False, f"invalid optional contract ID: {err}")
            elif cid in seen_addresses:
                record(
                    f"contracts.{ckey}",
                    False,
                    f"duplicate address {cid} already assigned to {seen_addresses[cid]}",
                )
            else:
                seen_addresses[cid] = ckey
                record(f"contracts.{ckey}", True, cid)

    assets = manifest.get("assets")
    if not isinstance(assets, dict):
        record("assets.map", False, "manifest.assets must be an object")
    else:
        usdc = str(assets.get("usdc", "")).strip()
        ok, err = validate_strkey(usdc)
        record("assets.usdc", ok, usdc if ok else f"invalid USDC asset StrKey: {err}")
        xlm = str(assets.get("xlm", "") or "").strip()
        if xlm:
            ok_x, err_x = validate_strkey(xlm)
            record("assets.xlm", ok_x, xlm if ok_x else f"invalid XLM asset StrKey: {err_x}")

    # 5. Cross-check .env, .env.deployed, deployments.json if provided
    env_vals: dict[str, str] = {}
    if env_file is not None:
        if not env_file.is_file():
            record("env.file", False, f".env file not found: {env_file}")
        else:
            env_vals = load_env_file(env_file)
            env_net = env_vals.get("STELLAR_NETWORK", "").strip()
            if env_net and env_net != m_net:
                record("env.network", False, f".env STELLAR_NETWORK={env_net!r} != manifest {m_net!r}")
            elif env_net:
                record("env.network", True, env_net)
            env_pp = env_vals.get("STELLAR_NETWORK_PASSPHRASE", "").strip()
            if env_pp and env_pp != actual_passphrase:
                record("env.passphrase", False, ".env STELLAR_NETWORK_PASSPHRASE disagrees with manifest")
            for url_key in ("STELLAR_RPC_URL", "HORIZON_URL"):
                uval = env_vals.get(url_key, "").strip()
                if uval:
                    uerr = _check_url_network(uval, m_net)
                    record(f"env.{url_key}", uerr is None, uerr or uval)
            if isinstance(contracts, dict):
                for ckey, env_var in ENV_CONTRACT_KEYS.items():
                    ev = env_vals.get(env_var, "").strip()
                    mv = str(contracts.get(ckey, "") or "").strip()
                    if ev and mv and ev != mv:
                        record(f"env.{env_var}", False, f"drift: .env={ev} != manifest={mv}")
                    elif ckey in REQUIRED_CONTRACTS and ev == mv and mv:
                        record(f"env.{env_var}", True, "matches manifest")

    if env_deployed is not None:
        if not env_deployed.is_file():
            record("env_deployed.file", False, f"file not found: {env_deployed}")
        elif isinstance(contracts, dict):
            dep_vals = load_env_file(env_deployed)
            for ckey, dep_var in ENV_DEPLOYED_KEYS.items():
                dv = dep_vals.get(dep_var, "").strip()
                mv = str(contracts.get(ckey, "") or "").strip()
                if dv and mv and dv != mv:
                    record(f"env_deployed.{dep_var}", False, f"drift: {dep_var}={dv} != manifest={mv}")
                elif ckey in REQUIRED_CONTRACTS and dv == mv and mv:
                    record(f"env_deployed.{dep_var}", True, "matches manifest")

    if deployments_json is not None:
        if not deployments_json.is_file():
            record("deployments_json.file", False, f"file not found: {deployments_json}")
        elif isinstance(contracts, dict):
            dep_json = json.loads(deployments_json.read_text(encoding="utf-8"))
            for ckey in REQUIRED_CONTRACTS:
                jv = str(dep_json.get(ckey, "")).strip()
                mv = str(contracts.get(ckey, "")).strip()
                ok_j = bool(jv) and jv == mv
                record(f"deployments_json.{ckey}", ok_j, "matches manifest" if ok_j else f"drift: {jv!r} != {mv!r}")

    # 6. Optional read-only on-chain verification
    if check_onchain and isinstance(contracts, dict) and isinstance(manifest_hashes, dict):
        eff_rpc = rpc_url or env_vals.get("STELLAR_RPC_URL", "").strip() or DEFAULT_RPC_URLS.get(m_net, "")
        hash_map = {
            "registry": "registry",
            "invoice": "invoice",
            "escrow_usdc": "escrow",
            "pool_usdc": "pool",
            "pool_factory": "pool_factory",
        }
        for ckey, wasm_key in hash_map.items():
            cid = str(contracts.get(ckey, "") or "").strip()
            if not cid:
                continue
            try:
                _, onchain_sha = fetch_contract_wasm_rpc(cid, eff_rpc)
                exp_sha = str(manifest_hashes.get(wasm_key, "")).lower()
                ok_h = onchain_sha.lower() == exp_sha
                record(
                    f"onchain.wasm_hash.{ckey}",
                    ok_h,
                    f"sha256:{onchain_sha[:16]}.." if ok_h else f"onchain={onchain_sha} != manifest={exp_sha}",
                )
            except Exception as exc:
                record(f"onchain.wasm_hash.{ckey}", False, str(exc))

        cli = find_stellar_cli()
        source_acct = env_vals.get("DEPLOYER_ACCOUNT", "deployer") if env_vals else "deployer"
        if cli:
            wiring_checks = [
                ("pool_usdc", "get_invoice_contract", str(contracts.get("invoice", ""))),
                ("pool_usdc", "get_escrow_contract", str(contracts.get("escrow_usdc", ""))),
                ("invoice", "get_pool_contract", str(contracts.get("pool_usdc", ""))),
                ("invoice", "get_escrow_contract", str(contracts.get("escrow_usdc", ""))),
                ("escrow_usdc", "get_pool_contract", str(contracts.get("pool_usdc", ""))),
                ("escrow_usdc", "get_invoice_contract", str(contracts.get("invoice", ""))),
            ]
            for ckey, method, expected in wiring_checks:
                cid = str(contracts.get(ckey, "") or "").strip()
                if not cid or not expected:
                    continue
                try:
                    actual = _invoke_view(cli, cid, source_acct, m_net, method)
                    record(
                        f"onchain.wiring.{ckey}.{method}",
                        actual == expected,
                        actual if actual == expected else f"expected {expected}, got {actual}",
                    )
                except Exception as exc:
                    record(f"onchain.wiring.{ckey}.{method}", False, str(exc))

    return results


def build_manifest(
    wasm_dir: Path,
    network: str,
    version: str,
    env_file: Path | None = None,
    deployments_json: Path | None = None,
) -> dict:
    """Assemble a release manifest dictionary from local WASMs and env/deployments."""
    if network not in NETWORK_PASSPHRASES:
        raise ValueError(f"unsupported network: {network!r}")
    hashes, wasm_errors = compute_wasm_hashes(wasm_dir)
    if wasm_errors:
        raise ValueError("; ".join(wasm_errors))

    contracts: dict[str, str] = {}
    assets: dict[str, str] = {}
    if env_file and env_file.is_file():
        env = load_env_file(env_file)
        for ckey, evar in ENV_CONTRACT_KEYS.items():
            if env.get(evar):
                contracts[ckey] = env[evar]
        if env.get("USDC_ISSUER"):
            assets["usdc"] = env["USDC_ISSUER"]
        if env.get("XLM_ASSET"):
            assets["xlm"] = env["XLM_ASSET"]

    if deployments_json and deployments_json.is_file():
        dep = json.loads(deployments_json.read_text(encoding="utf-8"))
        for ckey in (*REQUIRED_CONTRACTS, "escrow_xlm", "pool_xlm", "pool_factory"):
            if dep.get(ckey):
                contracts[ckey] = dep[ckey]

    return {
        "version": version,
        "network": network,
        "network_passphrase": NETWORK_PASSPHRASES[network],
        "rpc_url": DEFAULT_RPC_URLS[network],
        "contracts": contracts,
        "assets": assets,
        "wasm_hashes": hashes,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--manifest", type=Path, help="path to release manifest JSON")
    parser.add_argument("--wasm-dir", type=Path, default=DEFAULT_WASM_DIR, help="directory containing compiled .wasm files")
    parser.add_argument("--network", default=None, help="expected target network (testnet|mainnet|public|futurenet)")
    parser.add_argument("--env-file", type=Path, default=None, help="optional .env file to cross-check")
    parser.add_argument("--env-deployed", type=Path, default=None, help="optional .env.deployed file to cross-check")
    parser.add_argument("--deployments-json", type=Path, default=None, help="optional deployments.json to cross-check")
    parser.add_argument("--check-onchain", action="store_true", help="verify on-chain WASM hashes and reciprocal wiring")
    parser.add_argument("--check-wasm-only", action="store_true", help="validate compiled WASM artifacts and print SHA-256 hashes")
    parser.add_argument("--generate-manifest", type=Path, default=None, help="write a manifest JSON from WASMs and env")
    parser.add_argument("--version", default="0.1.0", help="release version string for --generate-manifest")
    parser.add_argument("--json", action="store_true", help="emit JSON results")
    args = parser.parse_args(argv)

    if args.check_wasm_only:
        hashes, errors = compute_wasm_hashes(args.wasm_dir)
        if args.json:
            print(json.dumps({"wasm_hashes": hashes, "errors": errors}, indent=2))
        else:
            for key, digest in sorted(hashes.items()):
                print(f"  [PASS] {key}: sha256:{digest}")
            for err in errors:
                print(f"  [FAIL] {err}")
        return 1 if errors else 0

    if args.generate_manifest is not None:
        net = args.network or "testnet"
        try:
            manifest = build_manifest(
                wasm_dir=args.wasm_dir,
                network=net,
                version=args.version,
                env_file=args.env_file,
                deployments_json=args.deployments_json,
            )
        except (OSError, ValueError, json.JSONDecodeError) as exc:
            print(f"Error generating manifest: {exc}", file=sys.stderr)
            return 2
        args.generate_manifest.parent.mkdir(parents=True, exist_ok=True)
        args.generate_manifest.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        print(f"Generated release manifest at {args.generate_manifest}")
        return 0

    if args.manifest is None:
        print("Error: --manifest is required (or pass --check-wasm-only / --generate-manifest)", file=sys.stderr)
        return 2

    if not args.manifest.is_file():
        print(f"Error: manifest file not found: {args.manifest}", file=sys.stderr)
        return 2

    try:
        manifest_data = json.loads(args.manifest.read_text(encoding="utf-8"))
        if not isinstance(manifest_data, dict):
            raise ValueError("manifest root must be a JSON object")
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"Error reading manifest {args.manifest}: {exc}", file=sys.stderr)
        return 2

    results = verify_manifest(
        manifest=manifest_data,
        wasm_dir=args.wasm_dir,
        target_network=args.network,
        env_file=args.env_file,
        env_deployed=args.env_deployed,
        deployments_json=args.deployments_json,
        check_onchain=args.check_onchain,
    )
    passed = sum(1 for _, ok, _ in results if ok)
    failed = len(results) - passed

    if args.json:
        print(
            json.dumps(
                {
                    "passed": passed,
                    "failed": failed,
                    "checks": [{"name": n, "ok": ok, "detail": d} for n, ok, d in results],
                },
                indent=2,
            )
        )
    else:
        print(f"Release checklist verification ({passed}/{len(results)} passed)")
        for name, ok, detail in results:
            tag = "[PASS]" if ok else "[FAIL]"
            print(f"  {tag} {name}: {detail}")

    return 1 if failed > 0 else 0


if __name__ == "__main__":
    raise SystemExit(main())
