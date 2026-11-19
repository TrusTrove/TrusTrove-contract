#!/usr/bin/env python3
"""Reject raw panic/unwrap/expect calls in Rust production source.

The checker is deliberately dependency-free. It masks comments and string
literals before matching calls, excludes standalone test/benchmark files and
inline #[cfg(test)] items, and permits only exact source expressions listed in
production_panic_allowlist.txt.
"""
from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTRACTS = ROOT / "contracts"
ALLOWLIST_PATH = ROOT / "scripts" / "production_panic_allowlist.txt"
PATTERNS = (
    ("panic", re.compile(r"\bpanic\s*!\s*\(")),
    ("unwrap", re.compile(r"\.\s*unwrap\s*\(")),
    ("expect", re.compile(r"\.\s*expect\s*\(")),
)


@dataclass(frozen=True)
class Violation:
    path: str
    line: int
    kind: str
    source_line: str


@dataclass(frozen=True)
class Allowance:
    path: str
    kind: str
    expression: str
    rationale: str


def mask_non_code(source: str) -> str:
    """Replace comments and literals with spaces while preserving newlines."""
    out = list(source)
    i = 0
    n = len(source)

    def blank(start: int, end: int) -> None:
        for pos in range(start, end):
            if source[pos] != "\n":
                out[pos] = " "

    while i < n:
        # Rust raw strings: r"...", r#"..."#, br###"..."###.
        raw_start = i
        prefix_end = i
        if source.startswith("br", i) or source.startswith("rb", i):
            prefix_end = i + 2
        elif source[i] == "r":
            prefix_end = i + 1
        if prefix_end > i and prefix_end < n:
            j = prefix_end
            while j < n and source[j] == "#":
                j += 1
            if j < n and source[j] == '"':
                hashes = source[prefix_end:j]
                closing = '"' + hashes
                end = source.find(closing, j + 1)
                end = n if end < 0 else end + len(closing)
                blank(raw_start, end)
                i = end
                continue

        if source.startswith("//", i):
            end = source.find("\n", i)
            end = n if end < 0 else end
            blank(i, end)
            i = end
            continue
        if source.startswith("/*", i):
            depth = 1
            j = i + 2
            while j < n and depth:
                if source.startswith("/*", j):
                    depth += 1
                    j += 2
                elif source.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(i, j)
            i = j
            continue
        if source[i] == '"':
            j = i + 1
            while j < n:
                if source[j] == "\\":
                    j += 2
                elif source[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            blank(i, min(j, n))
            i = min(j, n)
            continue
        i += 1
    return "".join(out)


def mask_cfg_test_items(code: str) -> str:
    """Mask complete inline items introduced by #[cfg(test)]."""
    out = list(code)
    cursor = 0
    while True:
        marker = code.find("#[cfg(test)]", cursor)
        if marker < 0:
            break
        brace = code.find("{", marker)
        semicolon = code.find(";", marker)
        if brace < 0 or (semicolon >= 0 and semicolon < brace):
            cursor = marker + len("#[cfg(test)]")
            continue
        depth = 0
        end = brace
        while end < len(code):
            if code[end] == "{":
                depth += 1
            elif code[end] == "}":
                depth -= 1
                if depth == 0:
                    end += 1
                    break
            end += 1
        for pos in range(marker, min(end, len(code))):
            if code[pos] != "\n":
                out[pos] = " "
        cursor = max(end, marker + len("#[cfg(test)]"))
    return "".join(out)


def scan_source(source: str, path: str) -> list[Violation]:
    code = mask_cfg_test_items(mask_non_code(source))
    original_lines = source.splitlines()
    found: list[Violation] = []
    for kind, pattern in PATTERNS:
        for match in pattern.finditer(code):
            line_number = code.count(chr(10), 0, match.start()) + 1
            original = original_lines[line_number - 1] if line_number <= len(original_lines) else ""
            found.append(Violation(path, line_number, kind, original.strip()))
    return sorted(found, key=lambda item: (item.line, item.kind))


def load_allowlist(path: Path = ALLOWLIST_PATH) -> list[Allowance]:
    entries: list[Allowance] = []
    if not path.exists():
        return entries
    for line_number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("|", 3)
        if len(parts) != 4 or not all(part.strip() for part in parts):
            raise ValueError(f"{path}:{line_number}: expected path|kind|exact-expression|rationale")
        if parts[1].strip() not in {"panic", "unwrap", "expect"}:
            raise ValueError(f"{path}:{line_number}: unknown rule {parts[1]!r}")
        entries.append(Allowance(*(part.strip() for part in parts)))
    return entries


def normalized(text: str) -> str:
    return re.sub(r"\s+", "", text)


def check_repository(root: Path = ROOT) -> tuple[list[Violation], list[str]]:
    allowlist_path = root / "scripts" / "production_panic_allowlist.txt"
    try:
        allowances = load_allowlist(allowlist_path)
    except (OSError, ValueError) as error:
        return [], [str(error)]

    violations: list[Violation] = []
    used_allowances: set[int] = set()
    if not CONTRACTS.exists():
        return [], [f"contract source directory not found: {CONTRACTS}"]

    for source_path in sorted((root / "contracts").glob("*/src/**/*.rs")):
        relative = source_path.relative_to(root).as_posix()
        parts = source_path.parts
        if source_path.name in {"test.rs", "tests.rs", "bench.rs"} or any(
            part in {"tests", "benches"} for part in parts
        ):
            continue
        try:
            source = source_path.read_text(encoding="utf-8")
        except OSError as error:
            return violations, [f"cannot read {relative}: {error}"]
        for item in scan_source(source, relative):
            match_index = next(
                (
                    index
                    for index, allowance in enumerate(allowances)
                    if index not in used_allowances
                    and allowance.path == item.path
                    and allowance.kind == item.kind
                    and normalized(allowance.expression) in normalized(item.source_line)
                ),
                None,
            )
            if match_index is None:
                violations.append(item)
            else:
                used_allowances.add(match_index)

    stale = [
        f"unused allowlist entry: {item.path}|{item.kind}|{item.expression}"
        for index, item in enumerate(allowances)
        if index not in used_allowances
    ]
    return violations, stale


def main() -> int:
    violations, errors = check_repository()
    for error in errors:
        print(f"ERROR: {error}", file=sys.stderr)
    for item in violations:
        print(f"{item.path}:{item.line}: prohibited {item.kind} call: {item.source_line}")
    if violations or errors:
        print(
            f"Production panic check failed: {len(violations)} violation(s), "
            f"{len(errors)} configuration error(s).",
            file=sys.stderr,
        )
        return 1
    print("Production panic check passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
