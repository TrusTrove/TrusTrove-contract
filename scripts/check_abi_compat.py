#!/usr/bin/env python3
"""Read-only ABI compatibility checker against deployed Stellar testnet contracts.

Compares local Soroban contract interfaces (from compiled release WASM specs
or Rust sources) against live deployed contracts (read-only via Soroban RPC
`getLedgerEntries` or `stellar contract fetch`), and reports breaking changes.

Exit codes:
    0 — All compared interfaces are backwards-compatible (or --report-only)
    1 — One or more ABI-incompatible changes detected
    2 — Invalid configuration, missing inputs, or fetch error
"""
from __future__ import annotations

import argparse
import json
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

from soroban_spec import (
    CONTRACTS,
    ROOT,
    ContractSpec,
    EnumSpec,
    FnSpec,
    StructSpec,
    UnionSpec,
    fetch_contract_wasm_rpc,
    fetch_contract_wasm_stellar,
    find_stellar_cli,
    load_env_file,
    parse_readme_addresses,
    parse_wasm_spec,
    read_local_specs,
    validate_strkey,
)

DEFAULT_WASM_DIR = ROOT / "target" / "wasm32v1-none" / "release"
DEFAULT_RPC_URLS = {
    "testnet": "https://soroban-testnet.stellar.org",
    "futurenet": "https://rpc-futurenet.stellar.org",
    "mainnet": "https://mainnet.sorobanrpc.com",
    "public": "https://mainnet.sorobanrpc.com",
}
ENV_KEY_BY_CONTRACT = {
    "registry": "REGISTRY_CONTRACT_ID",
    "invoice": "INVOICE_CONTRACT_ID",
    "escrow": "ESCROW_USDC_CONTRACT_ID",
    "pool": "POOL_USDC_CONTRACT_ID",
    "pool_factory": "POOL_FACTORY_CONTRACT_ID",
}


@dataclass(frozen=True)
class AbiFinding:
    contract: str
    severity: str  # "incompatible" | "additive" | "info"
    kind: str
    symbol: str
    detail: str


def _compare_structs(contract: str, deployed: StructSpec, local: StructSpec) -> list[AbiFinding]:
    findings: list[AbiFinding] = []
    dep_fields = {f.name: f.type.canonical_str() for f in deployed.fields}
    loc_fields = {f.name: f.type.canonical_str() for f in local.fields}
    for fname, ftype in sorted(dep_fields.items()):
        if fname not in loc_fields:
            findings.append(
                AbiFinding(contract, "incompatible", "struct_field_removed", f"{deployed.name}.{fname}", f"field removed (was {ftype})")
            )
        elif loc_fields[fname] != ftype:
            findings.append(
                AbiFinding(
                    contract,
                    "incompatible",
                    "struct_field_type_changed",
                    f"{deployed.name}.{fname}",
                    f"field type changed: deployed={ftype} vs local={loc_fields[fname]}",
                )
            )
    for fname, ftype in sorted(loc_fields.items()):
        if fname not in dep_fields:
            findings.append(
                AbiFinding(
                    contract,
                    "incompatible",
                    "struct_field_added",
                    f"{deployed.name}.{fname}",
                    f"field added ({ftype}) alters XDR struct layout",
                )
            )
    return findings


def _compare_unions(contract: str, deployed: UnionSpec, local: UnionSpec) -> list[AbiFinding]:
    findings: list[AbiFinding] = []
    dep_cases = {c.name: tuple(t.canonical_str() for t in c.types) for c in deployed.cases}
    loc_cases = {c.name: tuple(t.canonical_str() for t in c.types) for c in local.cases}
    for cname, ctypes in sorted(dep_cases.items()):
        if cname not in loc_cases:
            findings.append(
                AbiFinding(contract, "incompatible", "union_case_removed", f"{deployed.name}::{cname}", "union variant removed")
            )
        elif loc_cases[cname] != ctypes:
            findings.append(
                AbiFinding(
                    contract,
                    "incompatible",
                    "union_case_type_changed",
                    f"{deployed.name}::{cname}",
                    f"variant payload changed: deployed={ctypes} vs local={loc_cases[cname]}",
                )
            )
    return findings


def _compare_enums(contract: str, deployed: EnumSpec, local: EnumSpec) -> list[AbiFinding]:
    findings: list[AbiFinding] = []
    dep_cases = {c.name: c.value for c in deployed.cases}
    loc_cases = {c.name: c.value for c in local.cases}
    for cname, cval in sorted(dep_cases.items()):
        if cname not in loc_cases:
            findings.append(
                AbiFinding(contract, "incompatible", "enum_case_removed", f"{deployed.name}::{cname}", f"case removed (code={cval})")
            )
        elif loc_cases[cname] != cval:
            findings.append(
                AbiFinding(
                    contract,
                    "incompatible",
                    "enum_discriminant_changed",
                    f"{deployed.name}::{cname}",
                    f"discriminant changed: deployed={cval} vs local={loc_cases[cname]}",
                )
            )
    return findings


def compare_contract_specs(contract: str, deployed: ContractSpec, local: ContractSpec) -> list[AbiFinding]:
    """Compare deployed ABI against local ABI and return sorted compatibility findings."""
    findings: list[AbiFinding] = []
    dep_fns: dict[str, FnSpec] = {f.name: f for f in deployed.functions}
    loc_fns: dict[str, FnSpec] = {f.name: f for f in local.functions}

    for fname in sorted(dep_fns):
        dfn = dep_fns[fname]
        if fname not in loc_fns:
            findings.append(
                AbiFinding(
                    contract,
                    "incompatible",
                    "removed_function",
                    fname,
                    f"deployed entrypoint '{fname}' is missing in local spec",
                )
            )
            continue
        lfn = loc_fns[fname]
        if len(dfn.inputs) != len(lfn.inputs):
            dep_sig = ", ".join(f"{p.name}: {p.type.canonical_str()}" for p in dfn.inputs)
            loc_sig = ", ".join(f"{p.name}: {p.type.canonical_str()}" for p in lfn.inputs)
            findings.append(
                AbiFinding(
                    contract,
                    "incompatible",
                    "arity_changed",
                    fname,
                    f"parameter count changed ({len(dfn.inputs)} -> {len(lfn.inputs)}): ({dep_sig}) vs ({loc_sig})",
                )
            )
        else:
            for idx, (dp, lp) in enumerate(zip(dfn.inputs, lfn.inputs)):
                dtype = dp.type.canonical_str()
                ltype = lp.type.canonical_str()
                if dtype != ltype:
                    findings.append(
                        AbiFinding(
                            contract,
                            "incompatible",
                            "param_type_changed",
                            f"{fname}({dp.name})",
                            f"arg #{idx} type changed: deployed={dtype} vs local={ltype}",
                        )
                    )
                elif dp.name != lp.name:
                    findings.append(
                        AbiFinding(
                            contract,
                            "info",
                            "param_renamed",
                            fname,
                            f"arg #{idx} renamed from '{dp.name}' to '{lp.name}' (type {dtype} unchanged)",
                        )
                    )
        dout = dfn.output.canonical_str()
        lout = lfn.output.canonical_str()
        if dout != lout:
            findings.append(
                AbiFinding(
                    contract,
                    "incompatible",
                    "return_type_changed",
                    fname,
                    f"return type changed: deployed={dout} vs local={lout}",
                )
            )

    for fname in sorted(loc_fns):
        if fname not in dep_fns:
            lfn = loc_fns[fname]
            sig = ", ".join(f"{p.name}: {p.type.canonical_str()}" for p in lfn.inputs)
            findings.append(
                AbiFinding(
                    contract,
                    "additive",
                    "added_function",
                    fname,
                    f"new entrypoint: {fname}({sig}) -> {lfn.output.canonical_str()}",
                )
            )

    loc_structs = {s.name: s for s in local.structs}
    for ds in sorted(deployed.structs, key=lambda s: s.name):
        if ds.name not in loc_structs:
            findings.append(AbiFinding(contract, "incompatible", "struct_removed", ds.name, "struct removed"))
        else:
            findings.extend(_compare_structs(contract, ds, loc_structs[ds.name]))

    loc_unions = {u.name: u for u in local.unions}
    for du in sorted(deployed.unions, key=lambda u: u.name):
        if du.name not in loc_unions:
            findings.append(AbiFinding(contract, "incompatible", "union_removed", du.name, "union removed"))
        else:
            findings.extend(_compare_unions(contract, du, loc_unions[du.name]))

    loc_enums = {e.name: e for e in local.enums}
    for de in sorted(deployed.enums, key=lambda e: e.name):
        if de.name not in loc_enums:
            findings.append(AbiFinding(contract, "incompatible", "enum_removed", de.name, "enum removed"))
        else:
            findings.extend(_compare_enums(contract, de, loc_enums[de.name]))

    return findings


def resolve_contract_addresses(
    cli_contracts: list[str],
    env_file: Path | None,
    readme_path: Path | None,
) -> dict[str, str]:
    """Resolve deployed contract IDs from README, .env, and CLI overrides."""
    resolved: dict[str, str] = {}
    if readme_path and readme_path.is_file():
        resolved.update(parse_readme_addresses(readme_path))
    if env_file and env_file.is_file():
        env = load_env_file(env_file)
        for key, env_var in ENV_KEY_BY_CONTRACT.items():
            val = env.get(env_var, "").strip()
            if val:
                resolved[key] = val

    known_keys = {c[0] for c in CONTRACTS}
    for item in cli_contracts:
        if "=" not in item:
            raise ValueError(f"invalid --contract value {item!r}; expected key=CONTRACT_ID")
        key, cid = item.split("=", 1)
        key = key.strip()
        cid = cid.strip()
        if key not in known_keys:
            raise ValueError(f"unknown contract key {key!r}; expected one of {sorted(known_keys)}")
        resolved[key] = cid

    for key, cid in resolved.items():
        ok, err = validate_strkey(cid, expected_prefix="C")
        if not ok:
            raise ValueError(f"invalid contract ID for '{key}': {cid!r} ({err})")

    return resolved


def fetch_deployed_spec(
    key: str,
    pascal: str,
    wasm_name: str,
    contract_id: str | None,
    network: str,
    rpc_url: str,
    mode: str,
    remote_wasm_dir: Path | None,
) -> tuple[ContractSpec, str]:
    """Fetch deployed contract spec via remote_wasm_dir, stellar CLI, or Soroban RPC."""
    if remote_wasm_dir is not None:
        wasm_path = remote_wasm_dir / wasm_name
        if not wasm_path.is_file():
            raise FileNotFoundError(f"remote baseline WASM not found: {wasm_path}")
        return parse_wasm_spec(wasm_path.read_bytes()).canonical(pascal), str(wasm_path)

    if not contract_id:
        raise ValueError(f"no deployed address configured for '{key}'")

    chosen = mode
    if chosen == "auto":
        chosen = "stellar" if find_stellar_cli() else "rpc"

    if chosen == "stellar":
        wasm_bytes, sha = fetch_contract_wasm_stellar(contract_id, network)
        return parse_wasm_spec(wasm_bytes).canonical(pascal), f"{contract_id} (sha256:{sha[:12]}.. via stellar)"
    if chosen == "rpc":
        wasm_bytes, sha = fetch_contract_wasm_rpc(contract_id, rpc_url)
        return parse_wasm_spec(wasm_bytes).canonical(pascal), f"{contract_id} (sha256:{sha[:12]}.. via rpc)"
    raise ValueError(f"unsupported fetch mode: {chosen!r}")


def format_markdown_summary(
    network: str,
    checked: list[tuple[str, str]],
    findings: list[AbiFinding],
) -> str:
    incompatible = [f for f in findings if f.severity == "incompatible"]
    additive = [f for f in findings if f.severity == "additive"]
    lines = [
        f"## ABI Compatibility Report (`{network}`)",
        "",
        f"- Contracts checked: {len(checked)}",
        f"- Incompatible changes: **{len(incompatible)}**",
        f"- Additive (backwards-compatible) changes: {len(additive)}",
        "",
        "| Contract | Target |",
        "|---|---|",
    ]
    for key, target in checked:
        lines.append(f"| `{key}` | `{target}` |")

    if incompatible:
        lines.extend(["", "### Incompatible Changes", "", "| Contract | Kind | Symbol | Detail |", "|---|---|---|---|"])
        for f in incompatible:
            lines.append(f"| `{f.contract}` | `{f.kind}` | `{f.symbol}` | {f.detail} |")
    if additive:
        lines.extend(["", "### Additive Changes", "", "| Contract | Symbol | Detail |", "|---|---|---|"])
        for f in additive:
            lines.append(f"| `{f.contract}` | `{f.symbol}` | {f.detail} |")
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--network", default="testnet", help="Stellar network name (default: testnet)")
    parser.add_argument("--rpc-url", default="", help="Soroban RPC URL override")
    parser.add_argument("--env-file", type=Path, default=ROOT / ".env", help="optional .env file path")
    parser.add_argument("--readme", type=Path, default=ROOT / "README.md", help="README.md for canonical addresses")
    parser.add_argument("--contract", action="append", default=[], help="override address as key=CONTRACT_ID")
    parser.add_argument("--wasm-dir", type=Path, default=DEFAULT_WASM_DIR, help="local compiled WASM directory")
    parser.add_argument("--from-source", action="store_true", help="read local ABI from Rust sources")
    parser.add_argument("--mode", choices=("auto", "rpc", "stellar"), default="auto", help="remote WASM fetch mode")
    parser.add_argument("--remote-wasm-dir", type=Path, default=None, help="compare against local baseline WASMs")
    parser.add_argument("--report-only", action="store_true", help="always exit 0 even on incompatible changes")
    parser.add_argument("--json", action="store_true", help="emit JSON report to stdout")
    parser.add_argument("--summary-file", type=Path, default=None, help="write Markdown report to file")
    args = parser.parse_args(argv)

    rpc_url = args.rpc_url.strip() or DEFAULT_RPC_URLS.get(args.network, "")
    if args.env_file.is_file() and not args.rpc_url.strip():
        env_vals = load_env_file(args.env_file)
        if env_vals.get("STELLAR_RPC_URL"):
            rpc_url = env_vals["STELLAR_RPC_URL"]

    try:
        addresses = resolve_contract_addresses(
            cli_contracts=args.contract,
            env_file=args.env_file if args.env_file.is_file() else None,
            readme_path=args.readme if args.readme.is_file() else None,
        )
        if not addresses and args.remote_wasm_dir is None:
            raise ValueError("no deployed contract addresses found (provide --contract, .env, or README.md)")
        local_specs = read_local_specs(wasm_dir=args.wasm_dir, from_source=args.from_source)
    except (FileNotFoundError, OSError, ValueError) as exc:
        print(f"Configuration error: {exc}", file=sys.stderr)
        return 2

    all_findings: list[AbiFinding] = []
    checked: list[tuple[str, str]] = []

    for key, pascal, wasm_name, _ in CONTRACTS:
        if args.remote_wasm_dir is None and key not in addresses:
            continue
        try:
            deployed_spec, source_desc = fetch_deployed_spec(
                key=key,
                pascal=pascal,
                wasm_name=wasm_name,
                contract_id=addresses.get(key),
                network=args.network,
                rpc_url=rpc_url,
                mode=args.mode,
                remote_wasm_dir=args.remote_wasm_dir,
            )
        except (FileNotFoundError, RuntimeError, ValueError) as exc:
            print(f"Fetch error for '{key}': {exc}", file=sys.stderr)
            return 2

        checked.append((key, source_desc))
        all_findings.extend(compare_contract_specs(key, deployed_spec, local_specs[key]))

    if not checked:
        print("Configuration error: zero contracts selected for comparison", file=sys.stderr)
        return 2

    incompatible = [f for f in all_findings if f.severity == "incompatible"]
    additive = [f for f in all_findings if f.severity == "additive"]

    if args.summary_file:
        args.summary_file.parent.mkdir(parents=True, exist_ok=True)
        args.summary_file.write_text(format_markdown_summary(args.network, checked, all_findings), encoding="utf-8")

    if args.json:
        print(
            json.dumps(
                {
                    "network": args.network,
                    "checked": [{"contract": k, "target": t} for k, t in checked],
                    "compatible": len(incompatible) == 0,
                    "incompatible_count": len(incompatible),
                    "additive_count": len(additive),
                    "findings": [asdict(f) for f in all_findings],
                },
                indent=2,
            )
        )
    else:
        print(f"ABI compatibility check — network: {args.network} ({len(checked)} contract(s))")
        for key, target in checked:
            c_inc = [f for f in incompatible if f.contract == key]
            c_add = [f for f in additive if f.contract == key]
            status = "[FAIL]" if c_inc else "[PASS]"
            print(f"  {status} {key} ({target}): {len(c_inc)} incompatible, {len(c_add)} additive")
            for f in c_inc:
                print(f"    - [{f.kind}] {f.symbol}: {f.detail}")
        if incompatible:
            print(f"Summary: {len(incompatible)} incompatible ABI change(s) detected.")
        else:
            print("Summary: all checked contracts are ABI-compatible.")

    if incompatible and not args.report_only:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
