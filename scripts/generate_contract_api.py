#!/usr/bin/env python3
"""Generate and verify the canonical Soroban contract API table."""
from __future__ import annotations
import argparse, re, sys
from pathlib import Path
from check_production_panic_calls import mask_non_code

ROOT = Path(__file__).resolve().parents[1]
README = ROOT / "README.md"
CONTRACTS = (
    ("Registry", "contracts/registry/src/lib.rs"),
    ("Invoice", "contracts/invoice/src/lib.rs"),
    ("Escrow", "contracts/escrow/src/lib.rs"),
    ("Pool", "contracts/pool/src/lib.rs"),
    ("Pool Factory", "contracts/pool_factory/src/lib.rs"),
)
BEGIN = "<!-- BEGIN GENERATED CONTRACT API -->"
END = "<!-- END GENERATED CONTRACT API -->"
BT = chr(96)

def matching(code, opening, left, right):
    depth = 0
    for i in range(opening, len(code)):
        if code[i] == left: depth += 1
        elif code[i] == right:
            depth -= 1
            if depth == 0: return i
    raise ValueError(f"unbalanced {left!r} delimiter at offset {opening}")

def split_top_level(text):
    result, start = [], 0
    angle = paren = square = brace = 0
    for i, char in enumerate(text):
        if char == "<": angle += 1
        elif char == ">": angle -= 1
        elif char == "(": paren += 1
        elif char == ")": paren -= 1
        elif char == "[": square += 1
        elif char == "]": square -= 1
        elif char == "{": brace += 1
        elif char == "}": brace -= 1
        elif char == "," and angle == paren == square == brace == 0:
            if text[start:i].strip(): result.append(text[start:i].strip())
            start = i + 1
    if text[start:].strip(): result.append(text[start:].strip())
    if min(angle, paren, square, brace) < 0 or any((angle, paren, square, brace)):
        raise ValueError(f"unbalanced Rust signature: {text!r}")
    return result

def normalize(text):
    text = re.sub(r"\s+", " ", text).strip()
    text = re.sub(r"\s*([,>])\s*", r"\1", text)
    text = re.sub(r"<\s*", "<", text)
    text = re.sub(r"\(\s*", "(", text)
    text = re.sub(r"\s*\)", ")", text)
    return re.sub(r",\s*", ", ", text)

def parse_contract_signatures(source, source_name="<source>"):
    code, signatures, cursor = mask_non_code(source), [], 0
    marker = "#[contractimpl]"
    while True:
        pos = code.find(marker, cursor)
        if pos < 0: break
        impl = re.search(r"\bimpl\b", code[pos + len(marker):])
        if not impl: raise ValueError(f"{source_name}: contractimpl is not followed by impl")
        impl_pos = pos + len(marker) + impl.start()
        brace = code.find("{", impl_pos)
        if brace < 0: raise ValueError(f"{source_name}: impl has no opening brace")
        end = matching(code, brace, "{", "}")
        block = code[brace + 1:end]
        pattern = re.compile(r"\bpub\s+fn\s+([A-Za-z_]\w*)\s*\(")
        for match in pattern.finditer(block):
            opening = code.find("(", brace + 1 + match.start(), brace + 1 + match.end())
            if opening < 0: raise ValueError(f"{source_name}: cannot parse {match.group(1)} args")
            closing = matching(code, opening, "(", ")")
            args = []
            for arg in split_top_level(source[opening + 1:closing]):
                arg = normalize(arg)
                arg = re.sub(r"^([A-Za-z_]\w*)\s*:\s*", r"\1: ", arg)
                if re.fullmatch(r"_?env:\s*(?:&\s*)?Env", arg): continue
                if ":" not in arg:
                    raise ValueError(f"{source_name}: unsupported parameter in {match.group(1)}: {arg!r}")
                args.append(arg)
            tail = closing + 1
            while tail < end and code[tail].isspace(): tail += 1
            ret = "()"
            if code.startswith("->", tail):
                start, p = tail + 2, tail + 2
                angle = paren = square = 0
                while p < end:
                    ch = code[p]
                    if ch == "<": angle += 1
                    elif ch == ">": angle -= 1
                    elif ch == "(": paren += 1
                    elif ch == ")": paren -= 1
                    elif ch == "[": square += 1
                    elif ch == "]": square -= 1
                    elif ch == "{" and angle == paren == square == 0: break
                    p += 1
                if p >= end or min(angle, paren, square) < 0 or any((angle, paren, square)):
                    raise ValueError(f"{source_name}: cannot parse return type for {match.group(1)}")
                ret = normalize(source[start:p])
                if not ret: raise ValueError(f"{source_name}: empty return type for {match.group(1)}")
            signatures.append(f"{match.group(1)}({', '.join(args)}) -> {ret}")
        cursor = end + 1
    if not signatures: raise ValueError(f"{source_name}: no public methods in contractimpl blocks")
    return signatures

def generate_section(root=ROOT):
    lines = [
        BEGIN, "### Canonical public contract API", "",
        "Generated from current contractimpl declarations by python3 scripts/generate_contract_api.py --write.",
        "These are client-facing signatures: the framework-provided env: Env parameter is omitted; all other argument names/types and return types are taken from the Rust declarations. Methods without an explicit return type are shown as returning ().",
        "", "| Contract | Public function | Client-facing signature |", "|---|---|---|",
    ]
    total = 0
    for contract, rel in CONTRACTS:
        try: source = (root / rel).read_text(encoding="utf-8")
        except OSError as e: raise ValueError(f"cannot read {rel}: {e}") from e
        for sig in parse_contract_signatures(source, rel):
            name = sig.split("(", 1)[0]
            safe_sig = sig.replace("|", chr(92) + "|")
            lines.append(f"| {contract} | {BT}{name}{BT} | {BT}{safe_sig}{BT} |")
            total += 1
    lines += [
        "", f"**Total: {total} public entry points across five deployable contracts.**", "",
        "The shared TTL crate (contracts/ttl/src/lib.rs) exports only the THRESHOLD and EXTEND_TO constants. It is a workspace utility, not a callable contract API, so it has no entry-point group in this table.",
        END,
    ]
    return "\n".join(lines)

def replace_section(readme, generated):
    if readme.count(BEGIN) != 1 or readme.count(END) != 1:
        raise ValueError("README.md must contain exactly one generated API begin/end marker pair")
    start, finish = readme.index(BEGIN), readme.index(END)
    if finish < start: raise ValueError("README.md generated API end marker precedes its begin marker")
    return readme[:start] + generated + readme[finish + len(END):]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--write", action="store_true", help="regenerate the README API table")
    group.add_argument("--check", action="store_true", help="fail if the README API table is stale")
    args = parser.parse_args()
    try:
        generated = generate_section()
        current = README.read_text(encoding="utf-8")
        updated = replace_section(current, generated)
    except (OSError, ValueError) as error:
        print(f"API generation failed: {error}", file=sys.stderr)
        return 1
    if args.write:
        README.write_text(updated, encoding="utf-8")
        print("Updated canonical contract API table in README.md.")
        return 0
    if current != updated:
        print("README.md canonical contract API table is stale; run python3 scripts/generate_contract_api.py --write.")
        return 1
    print("Canonical contract API table is up to date.")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
