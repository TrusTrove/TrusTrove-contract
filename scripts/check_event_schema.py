#!/usr/bin/env python3
"""Capture and verify the on-chain event schema of every contract.

Every `pub fn` in a contract's `events.rs` is one event emitter. For each one
we record its typed parameters and the whitespace-normalized
`env.events().publish(topics, data)` call, which is exactly what fixes the
event's topic layout and data shape on the wire. The result is stored in
`scripts/fixtures/event_schemas.json`; `--check` fails when the source no
longer matches the reviewed fixture, so a topic/data change cannot land
without an explicit fixture update in the same pull request.
"""
from __future__ import annotations
import argparse, json, re, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = ROOT / "scripts" / "fixtures" / "event_schemas.json"
CONTRACTS = ("registry", "invoice", "escrow", "pool", "pool_factory")


def strip_comments(source):
    return re.sub(r"//[^\n]*", "", source)


def matching(text, opening, left, right):
    depth = 0
    for i in range(opening, len(text)):
        if text[i] == left:
            depth += 1
        elif text[i] == right:
            depth -= 1
            if depth == 0:
                return i
    raise ValueError(f"unbalanced {left!r} at offset {opening}")


def squash(text):
    return re.sub(r"\s+", "", text)


def parse_events(source, name="<source>"):
    """Return {emitter_fn: {"params": [...], "publish": [...]}}."""
    code = strip_comments(source)
    events = {}
    for m in re.finditer(r"\bpub\s+fn\s+([A-Za-z_]\w*)\s*\(", code):
        fn = m.group(1)
        opening = m.end() - 1
        closing = matching(code, opening, "(", ")")
        params = [squash(p) for p in re.split(r",(?![^<(\[]*[>)\]])", code[opening + 1:closing]) if p.strip()]
        params = [p for p in params if not re.fullmatch(r"_?env:&?Env", p)]
        brace = code.find("{", closing)
        if brace < 0:
            raise ValueError(f"{name}: {fn} has no body")
        body = code[brace + 1:matching(code, brace, "{", "}")]
        publishes = []
        for p in re.finditer(r"\.publish\s*\(", body):
            op = p.end() - 1
            publishes.append(squash(body[op + 1:matching(body, op, "(", ")")]))
        if publishes:
            events[fn] = {"params": params, "publish": publishes}
    return events


def collect(root=ROOT):
    schema = {}
    for contract in CONTRACTS:
        path = root / "contracts" / contract / "src" / "events.rs"
        try:
            source = path.read_text(encoding="utf-8")
        except OSError as e:
            raise ValueError(f"cannot read {path}: {e}") from e
        events = parse_events(source, str(path))
        if not events:
            raise ValueError(f"{path}: no event emitters found")
        schema[contract] = events
    return schema


def render(schema):
    return json.dumps(schema, indent=2, sort_keys=True) + "\n"


def diff(old, new):
    problems = []
    for contract in sorted(set(old) | set(new)):
        o, n = old.get(contract, {}), new.get(contract, {})
        for fn in sorted(set(o) | set(n)):
            if fn not in o:
                problems.append(f"{contract}::{fn}: new event not in fixture")
            elif fn not in n:
                problems.append(f"{contract}::{fn}: event removed from source")
            elif o[fn] != n[fn]:
                problems.append(f"{contract}::{fn}: schema changed\n    fixture: {o[fn]}\n    source:  {n[fn]}")
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--write", action="store_true", help="regenerate the reviewed fixture")
    group.add_argument("--check", action="store_true", help="fail on unreviewed schema differences")
    args = parser.parse_args()
    try:
        current = collect()
    except ValueError as error:
        print(f"Event schema capture failed: {error}", file=sys.stderr)
        return 1
    if args.write:
        FIXTURE.parent.mkdir(parents=True, exist_ok=True)
        FIXTURE.write_text(render(current), encoding="utf-8")
        print(f"Wrote {FIXTURE.relative_to(ROOT)}.")
        return 0
    if not FIXTURE.exists():
        print("Event schema fixture missing; run python3 scripts/check_event_schema.py --write and review it.")
        return 1
    problems = diff(json.loads(FIXTURE.read_text(encoding="utf-8")), current)
    if problems:
        print("Unreviewed event schema differences:")
        for p in problems:
            print(f"  - {p}")
        print("If intentional, notify indexer owners, update docs/EVENTS.md and run --write.")
        return 1
    print("Event schema matches the reviewed fixture.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
