#!/usr/bin/env python3
"""Shared Soroban WASM spec, Rust source spec, StrKey, and RPC utilities.

Dependency-free helpers shared by the TypeScript bindings generator (#1205),
testnet ABI compatibility checker (#1206), and release checklist verifier (#1212).
"""
from __future__ import annotations

import base64
import hashlib
import json
import re
import shutil
import struct
import subprocess
import tempfile
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

from check_production_panic_calls import mask_cfg_test_items, mask_non_code
from generate_contract_api import matching, parse_contract_signatures, split_top_level

ROOT = Path(__file__).resolve().parents[1]

# (key, PascalName, wasm_filename, crate_rel_path)
CONTRACTS: tuple[tuple[str, str, str, str], ...] = (
    ("registry", "Registry", "trusttrove_registry.wasm", "contracts/registry"),
    ("invoice", "Invoice", "trusttrove_invoice.wasm", "contracts/invoice"),
    ("escrow", "Escrow", "trusttrove_escrow.wasm", "contracts/escrow"),
    ("pool", "Pool", "trusttrove_pool.wasm", "contracts/pool"),
    ("pool_factory", "PoolFactory", "trusttrove_pool_factory.wasm", "contracts/pool_factory"),
)

WASM_MAGIC = b"\x00asm\x01\x00\x00\x00"

# Stellar-contract-spec.x SCSpecType discriminants
_SPEC_PRIMITIVES_BY_CODE: dict[int, str] = {
    0: "Val",
    1: "bool",
    2: "void",
    3: "Error",
    4: "u32",
    5: "i32",
    6: "u64",
    7: "i64",
    8: "Timepoint",
    9: "Duration",
    10: "u128",
    11: "i128",
    12: "u256",
    13: "i256",
    14: "Bytes",
    16: "String",
    17: "Symbol",
    19: "Address",
    20: "MuxedAddress",
}
_SPEC_CODE_BY_PRIMITIVE: dict[str, int] = {v: k for k, v in _SPEC_PRIMITIVES_BY_CODE.items()}
_SPEC_CODE_BY_PRIMITIVE["()"] = 2

_STRKEY_VERSION_BYTES: dict[str, int] = {
    "G": 6 << 3,   # Ed25519 public key (48)
    "C": 2 << 3,   # Contract ID (16)
}


@dataclass(frozen=True)
class TypeRef:
    """Canonical representation of a Soroban SCSpecTypeDef."""

    kind: str  # primitive | option | result | vec | map | tuple | bytes_n | udt
    name: str = ""
    inner: TypeRef | None = None
    key: TypeRef | None = None
    value: TypeRef | None = None
    ok: TypeRef | None = None
    err: TypeRef | None = None
    elements: tuple[TypeRef, ...] = ()
    length: int | None = None

    def canonical_str(self) -> str:
        if self.kind == "primitive":
            return "()" if self.name == "void" else self.name
        if self.kind == "option":
            assert self.inner is not None
            return f"Option<{self.inner.canonical_str()}>"
        if self.kind == "result":
            assert self.ok is not None and self.err is not None
            return f"Result<{self.ok.canonical_str()}, {self.err.canonical_str()}>"
        if self.kind == "vec":
            assert self.inner is not None
            return f"Vec<{self.inner.canonical_str()}>"
        if self.kind == "map":
            assert self.key is not None and self.value is not None
            return f"Map<{self.key.canonical_str()}, {self.value.canonical_str()}>"
        if self.kind == "tuple":
            return "(" + ", ".join(el.canonical_str() for el in self.elements) + ")"
        if self.kind == "bytes_n":
            return f"BytesN<{self.length}>"
        if self.kind == "udt":
            return self.name
        raise ValueError(f"unknown TypeRef kind: {self.kind}")

    def referenced_udts(self) -> set[str]:
        if self.kind == "udt":
            return {self.name}
        if self.kind in {"option", "vec"} and self.inner is not None:
            return self.inner.referenced_udts()
        if self.kind == "map" and self.key is not None and self.value is not None:
            return self.key.referenced_udts() | self.value.referenced_udts()
        if self.kind == "result" and self.ok is not None and self.err is not None:
            return self.ok.referenced_udts() | self.err.referenced_udts()
        if self.kind == "tuple":
            out: set[str] = set()
            for el in self.elements:
                out |= el.referenced_udts()
            return out
        return set()


@dataclass(frozen=True)
class FnParam:
    name: str
    type: TypeRef


@dataclass(frozen=True)
class FnSpec:
    name: str
    inputs: tuple[FnParam, ...]
    output: TypeRef


@dataclass(frozen=True)
class StructField:
    name: str
    type: TypeRef


@dataclass(frozen=True)
class StructSpec:
    name: str
    fields: tuple[StructField, ...]


@dataclass(frozen=True)
class UnionCase:
    name: str
    types: tuple[TypeRef, ...]


@dataclass(frozen=True)
class UnionSpec:
    name: str
    cases: tuple[UnionCase, ...]


@dataclass(frozen=True)
class EnumCase:
    name: str
    value: int


@dataclass(frozen=True)
class EnumSpec:
    name: str
    cases: tuple[EnumCase, ...]


@dataclass(frozen=True)
class ContractSpec:
    """Normalized Soroban contract interface specification."""

    functions: tuple[FnSpec, ...]
    structs: tuple[StructSpec, ...]
    unions: tuple[UnionSpec, ...]
    enums: tuple[EnumSpec, ...]
    error_enums: tuple[EnumSpec, ...]

    def canonical(self, contract_pascal_name: str | None = None) -> ContractSpec:
        """Return a deterministically sorted spec with only client-facing UDTs.

        Excludes internal storage enums (e.g. DataKey, PauseState) and shared
        workspace error enums (PauseError) so WASM-extracted and source-parsed
        specs match identically.
        """
        struct_map = {s.name: s for s in self.structs}
        union_map = {u.name: u for u in self.unions}
        enum_map = {e.name: e for e in self.enums}

        reachable: set[str] = set()
        queue: list[str] = []
        for fn in self.functions:
            for p in fn.inputs:
                queue.extend(p.type.referenced_udts())
            queue.extend(fn.output.referenced_udts())

        while queue:
            name = queue.pop()
            if name in reachable:
                continue
            reachable.add(name)
            if name in struct_map:
                for f in struct_map[name].fields:
                    queue.extend(f.type.referenced_udts())
            elif name in union_map:
                for c in union_map[name].cases:
                    for t in c.types:
                        queue.extend(t.referenced_udts())

        expected_error = f"{contract_pascal_name}Error" if contract_pascal_name else None
        filtered_errors = [
            EnumSpec(
                name=e.name,
                cases=tuple(sorted(e.cases, key=lambda c: (c.value, c.name))),
            )
            for e in self.error_enums
            if e.name != "PauseError" and (expected_error is None or e.name == expected_error)
        ]

        return ContractSpec(
            functions=tuple(sorted(self.functions, key=lambda f: f.name)),
            structs=tuple(
                StructSpec(name=s.name, fields=tuple(sorted(s.fields, key=lambda f: f.name)))
                for s in sorted((struct_map[n] for n in reachable if n in struct_map), key=lambda x: x.name)
            ),
            unions=tuple(
                UnionSpec(name=u.name, cases=tuple(sorted(u.cases, key=lambda c: c.name)))
                for u in sorted((union_map[n] for n in reachable if n in union_map), key=lambda x: x.name)
            ),
            enums=tuple(
                EnumSpec(name=e.name, cases=tuple(sorted(e.cases, key=lambda c: (c.value, c.name))))
                for e in sorted((enum_map[n] for n in reachable if n in enum_map), key=lambda x: x.name)
            ),
            error_enums=tuple(sorted(filtered_errors, key=lambda e: e.name)),
        )


def parse_rust_type(text: str) -> TypeRef:
    """Parse a normalized Rust Soroban type expression into a TypeRef."""
    t = text.strip()
    if t in {"()", "void"}:
        return TypeRef(kind="primitive", name="void")
    if t in _SPEC_CODE_BY_PRIMITIVE:
        return TypeRef(kind="primitive", name=t)
    if t.startswith("(") and t.endswith(")"):
        inner_items = split_top_level(t[1:-1])
        return TypeRef(kind="tuple", elements=tuple(parse_rust_type(item) for item in inner_items))
    if t.startswith("Option<") and t.endswith(">"):
        return TypeRef(kind="option", inner=parse_rust_type(t[7:-1]))
    if t.startswith("Vec<") and t.endswith(">"):
        return TypeRef(kind="vec", inner=parse_rust_type(t[4:-1]))
    if t.startswith("Map<") and t.endswith(">"):
        parts = split_top_level(t[4:-1])
        if len(parts) != 2:
            raise ValueError(f"invalid Map type: {t!r}")
        return TypeRef(kind="map", key=parse_rust_type(parts[0]), value=parse_rust_type(parts[1]))
    if t.startswith("Result<") and t.endswith(">"):
        parts = split_top_level(t[7:-1])
        if len(parts) != 2:
            raise ValueError(f"invalid Result type: {t!r}")
        return TypeRef(kind="result", ok=parse_rust_type(parts[0]), err=parse_rust_type(parts[1]))
    m = re.fullmatch(r"BytesN<(\d+)>", t)
    if m:
        return TypeRef(kind="bytes_n", length=int(m.group(1)))
    if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_:]*", t):
        return TypeRef(kind="udt", name=t.rsplit("::", 1)[-1])
    raise ValueError(f"unsupported Rust type expression: {t!r}")


# ---------------------------------------------------------------------------
# WASM + XDR contractspecv0 parser / encoder
# ---------------------------------------------------------------------------

def _read_leb128(data: bytes, pos: int) -> tuple[int, int]:
    result = 0
    shift = 0
    while True:
        if pos >= len(data):
            raise ValueError("unexpected EOF while reading LEB128")
        byte = data[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        if not (byte & 0x80):
            return result, pos
        shift += 7
        if shift > 35:
            raise ValueError("LEB128 integer overflow")


def _write_leb128(value: int) -> bytes:
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


class _XdrReader:
    def __init__(self, data: bytes) -> None:
        self.data = data
        self.pos = 0

    def remaining(self) -> int:
        return len(self.data) - self.pos

    def u32(self) -> int:
        if self.pos + 4 > len(self.data):
            raise ValueError("unexpected EOF reading XDR u32")
        val = struct.unpack(">I", self.data[self.pos : self.pos + 4])[0]
        self.pos += 4
        return val

    def opaque(self) -> bytes:
        length = self.u32()
        pad = (-length) % 4
        if self.pos + length + pad > len(self.data):
            raise ValueError("unexpected EOF reading XDR opaque/string")
        val = self.data[self.pos : self.pos + length]
        self.pos += length + pad
        return val

    def string(self) -> str:
        return self.opaque().decode("utf-8")

    def typedef(self) -> TypeRef:
        code = self.u32()
        if code in _SPEC_PRIMITIVES_BY_CODE:
            return TypeRef(kind="primitive", name=_SPEC_PRIMITIVES_BY_CODE[code])
        if code == 1000:
            return TypeRef(kind="option", inner=self.typedef())
        if code == 1001:
            return TypeRef(kind="result", ok=self.typedef(), err=self.typedef())
        if code == 1002:
            return TypeRef(kind="vec", inner=self.typedef())
        if code == 1004:
            return TypeRef(kind="map", key=self.typedef(), value=self.typedef())
        if code == 1005:
            count = self.u32()
            return TypeRef(kind="tuple", elements=tuple(self.typedef() for _ in range(count)))
        if code == 1006:
            return TypeRef(kind="bytes_n", length=self.u32())
        if code == 2000:
            return TypeRef(kind="udt", name=self.string())
        raise ValueError(f"unsupported SCSpecType code: {code}")


def _xdr_u32(val: int) -> bytes:
    return struct.pack(">I", val)


def _xdr_str(text: str) -> bytes:
    raw = text.encode("utf-8")
    pad = (-len(raw)) % 4
    return _xdr_u32(len(raw)) + raw + (b"\x00" * pad)


def _xdr_typedef(t: TypeRef) -> bytes:
    if t.kind == "primitive":
        return _xdr_u32(_SPEC_CODE_BY_PRIMITIVE[t.name])
    if t.kind == "option":
        assert t.inner is not None
        return _xdr_u32(1000) + _xdr_typedef(t.inner)
    if t.kind == "result":
        assert t.ok is not None and t.err is not None
        return _xdr_u32(1001) + _xdr_typedef(t.ok) + _xdr_typedef(t.err)
    if t.kind == "vec":
        assert t.inner is not None
        return _xdr_u32(1002) + _xdr_typedef(t.inner)
    if t.kind == "map":
        assert t.key is not None and t.value is not None
        return _xdr_u32(1004) + _xdr_typedef(t.key) + _xdr_typedef(t.value)
    if t.kind == "tuple":
        return _xdr_u32(1005) + _xdr_u32(len(t.elements)) + b"".join(_xdr_typedef(e) for e in t.elements)
    if t.kind == "bytes_n":
        assert t.length is not None
        return _xdr_u32(1006) + _xdr_u32(t.length)
    if t.kind == "udt":
        return _xdr_u32(2000) + _xdr_str(t.name)
    raise ValueError(f"cannot XDR-encode TypeRef kind: {t.kind}")


def encode_wasm_with_spec(spec: ContractSpec) -> bytes:
    """Encode a ContractSpec into a minimal valid WASM binary with contractspecv0."""
    buf = bytearray()
    for fn in spec.functions:
        buf += _xdr_u32(0)  # FunctionV0
        buf += _xdr_str("")  # doc
        buf += _xdr_str(fn.name)
        buf += _xdr_u32(len(fn.inputs))
        for p in fn.inputs:
            buf += _xdr_str("") + _xdr_str(p.name) + _xdr_typedef(p.type)
        if fn.output.kind == "primitive" and fn.output.name == "void":
            buf += _xdr_u32(0)
        else:
            buf += _xdr_u32(1) + _xdr_typedef(fn.output)
    for st in spec.structs:
        buf += _xdr_u32(1)  # UdtStructV0
        buf += _xdr_str("") + _xdr_str("") + _xdr_str(st.name)
        buf += _xdr_u32(len(st.fields))
        for f in st.fields:
            buf += _xdr_str("") + _xdr_str(f.name) + _xdr_typedef(f.type)
    for un in spec.unions:
        buf += _xdr_u32(2)  # UdtUnionV0
        buf += _xdr_str("") + _xdr_str("") + _xdr_str(un.name)
        buf += _xdr_u32(len(un.cases))
        for c in un.cases:
            if not c.types:
                buf += _xdr_u32(0) + _xdr_str("") + _xdr_str(c.name)
            else:
                buf += _xdr_u32(1) + _xdr_str("") + _xdr_str(c.name) + _xdr_u32(len(c.types))
                for t in c.types:
                    buf += _xdr_typedef(t)
    for en in spec.enums:
        buf += _xdr_u32(3)  # UdtEnumV0
        buf += _xdr_str("") + _xdr_str("") + _xdr_str(en.name)
        buf += _xdr_u32(len(en.cases))
        for c in en.cases:
            buf += _xdr_str("") + _xdr_str(c.name) + _xdr_u32(c.value)
    for err in spec.error_enums:
        buf += _xdr_u32(4)  # UdtErrorEnumV0
        buf += _xdr_str("") + _xdr_str("") + _xdr_str(err.name)
        buf += _xdr_u32(len(err.cases))
        for c in err.cases:
            buf += _xdr_str("") + _xdr_str(c.name) + _xdr_u32(c.value)

    sec_name = b"contractspecv0"
    payload = _write_leb128(len(sec_name)) + sec_name + bytes(buf)
    return WASM_MAGIC + b"\x00" + _write_leb128(len(payload)) + payload


def extract_contract_spec_bytes(wasm_bytes: bytes) -> bytes:
    """Extract concatenated `contractspecv0` custom section bytes from a WASM binary."""
    if len(wasm_bytes) < len(WASM_MAGIC) or wasm_bytes[: len(WASM_MAGIC)] != WASM_MAGIC:
        raise ValueError("invalid WASM header (expected \\0asm v1)")

    pos = len(WASM_MAGIC)
    spec_chunks: list[bytes] = []
    while pos < len(wasm_bytes):
        section_id = wasm_bytes[pos]
        pos += 1
        section_len, pos = _read_leb128(wasm_bytes, pos)
        end = pos + section_len
        if end > len(wasm_bytes):
            raise ValueError("truncated WASM section")
        if section_id == 0:
            name_len, name_pos = _read_leb128(wasm_bytes, pos)
            if name_pos + name_len > end:
                raise ValueError("truncated WASM custom section name")
            name = wasm_bytes[name_pos : name_pos + name_len].decode("utf-8", errors="replace")
            if name == "contractspecv0":
                spec_chunks.append(wasm_bytes[name_pos + name_len : end])
        pos = end

    if not spec_chunks:
        raise ValueError("WASM binary contains no 'contractspecv0' custom section")
    return b"".join(spec_chunks)


def parse_wasm_spec(wasm_bytes: bytes) -> ContractSpec:
    """Decode a Soroban WASM binary's `contractspecv0` custom section into ContractSpec."""
    reader = _XdrReader(extract_contract_spec_bytes(wasm_bytes))
    functions: list[FnSpec] = []
    structs: list[StructSpec] = []
    unions: list[UnionSpec] = []
    enums: list[EnumSpec] = []
    error_enums: list[EnumSpec] = []

    while reader.remaining() > 0:
        kind = reader.u32()
        if kind == 0:  # SC_SPEC_ENTRY_FUNCTION_V0
            _doc = reader.string()
            name = reader.string()
            in_count = reader.u32()
            inputs = []
            for _ in range(in_count):
                _pdoc = reader.string()
                pname = reader.string()
                ptype = reader.typedef()
                inputs.append(FnParam(name=pname, type=ptype))
            out_count = reader.u32()
            outputs = [reader.typedef() for _ in range(out_count)]
            output = outputs[0] if outputs else TypeRef(kind="primitive", name="void")
            functions.append(FnSpec(name=name, inputs=tuple(inputs), output=output))
        elif kind == 1:  # SC_SPEC_ENTRY_UDT_STRUCT_V0
            _doc = reader.string()
            _lib = reader.string()
            name = reader.string()
            fcount = reader.u32()
            fields = []
            for _ in range(fcount):
                _fdoc = reader.string()
                fname = reader.string()
                ftype = reader.typedef()
                fields.append(StructField(name=fname, type=ftype))
            structs.append(StructSpec(name=name, fields=tuple(fields)))
        elif kind == 2:  # SC_SPEC_ENTRY_UDT_UNION_V0
            _doc = reader.string()
            _lib = reader.string()
            name = reader.string()
            ccount = reader.u32()
            cases = []
            for _ in range(ccount):
                ckind = reader.u32()
                _cdoc = reader.string()
                cname = reader.string()
                if ckind == 0:
                    cases.append(UnionCase(name=cname, types=()))
                elif ckind == 1:
                    tcount = reader.u32()
                    ctypes = tuple(reader.typedef() for _ in range(tcount))
                    cases.append(UnionCase(name=cname, types=ctypes))
                else:
                    raise ValueError(f"unknown SCSpecUDTUnionCaseV0Kind: {ckind}")
            unions.append(UnionSpec(name=name, cases=tuple(cases)))
        elif kind == 3:  # SC_SPEC_ENTRY_UDT_ENUM_V0
            _doc = reader.string()
            _lib = reader.string()
            name = reader.string()
            ccount = reader.u32()
            cases = []
            for _ in range(ccount):
                _cdoc = reader.string()
                cname = reader.string()
                cval = reader.u32()
                cases.append(EnumCase(name=cname, value=cval))
            enums.append(EnumSpec(name=name, cases=tuple(cases)))
        elif kind == 4:  # SC_SPEC_ENTRY_UDT_ERROR_ENUM_V0
            _doc = reader.string()
            _lib = reader.string()
            name = reader.string()
            ccount = reader.u32()
            cases = []
            for _ in range(ccount):
                _cdoc = reader.string()
                cname = reader.string()
                cval = reader.u32()
                cases.append(EnumCase(name=cname, value=cval))
            error_enums.append(EnumSpec(name=name, cases=tuple(cases)))
        elif kind == 5:  # SC_SPEC_ENTRY_EVENT_V0
            _doc = reader.string()
            _lib = reader.string()
            _name = reader.string()
            pcount = reader.u32()
            for _ in range(pcount):
                _ = reader.string()
            par_count = reader.u32()
            for _ in range(par_count):
                _ = reader.string()
                _ = reader.string()
                _ = reader.typedef()
                _ = reader.u32()
            _data_format = reader.u32()
        else:
            raise ValueError(f"unknown SCSpecEntryKind: {kind}")

    return ContractSpec(
        functions=tuple(functions),
        structs=tuple(structs),
        unions=tuple(unions),
        enums=tuple(enums),
        error_enums=tuple(error_enums),
    )


# ---------------------------------------------------------------------------
# Rust source fallback parser (mirrors soroban-sdk #[contractimpl/type/error])
# ---------------------------------------------------------------------------

def _normalize_rust_type(text: str) -> str:
    text = re.sub(r"\s+", " ", text).strip()
    text = re.sub(r"\s*([,>])\s*", r"\1", text)
    text = re.sub(r"<\s*", "<", text)
    text = re.sub(r"\(\s*", "(", text)
    text = re.sub(r"\s*\)", ")", text)
    return re.sub(r",\s*", ", ", text)


def parse_rust_contract_spec(crate_dir: Path) -> ContractSpec:
    """Parse #[contractimpl], #[contracttype], and #[contracterror] from a crate."""
    src_dir = crate_dir / "src"
    if not src_dir.is_dir():
        raise ValueError(f"missing contract src directory: {src_dir}")

    lib_rs = src_dir / "lib.rs"
    functions: list[FnSpec] = []
    for sig in parse_contract_signatures(lib_rs.read_text(encoding="utf-8"), str(lib_rs)):
        name_part, ret_part = sig.split(" -> ", 1)
        fn_name = name_part.split("(", 1)[0]
        args_str = name_part[len(fn_name) + 1 : -1].strip()
        params: list[FnParam] = []
        if args_str:
            for arg in split_top_level(args_str):
                pname, ptype = arg.split(":", 1)
                params.append(FnParam(name=pname.strip(), type=parse_rust_type(ptype.strip())))
        functions.append(FnSpec(name=fn_name, inputs=tuple(params), output=parse_rust_type(ret_part)))

    structs: list[StructSpec] = []
    unions: list[UnionSpec] = []
    enums: list[EnumSpec] = []
    error_enums: list[EnumSpec] = []

    for rs_file in sorted(src_dir.glob("*.rs")):
        if rs_file.name in {"test.rs", "tests.rs", "bench.rs"}:
            continue
        raw = rs_file.read_text(encoding="utf-8")
        code = mask_cfg_test_items(mask_non_code(raw))

        for attr_name, is_error in (("#[contracttype]", False), ("#[contracterror]", True)):
            cursor = 0
            while True:
                pos = code.find(attr_name, cursor)
                if pos < 0:
                    break
                decl = re.search(r"\bpub\s+(struct|enum)\s+([A-Za-z_]\w*)\s*\{", code[pos + len(attr_name) :])
                if not decl:
                    cursor = pos + len(attr_name)
                    continue
                kind_kw, type_name = decl.group(1), decl.group(2)
                brace_open = pos + len(attr_name) + decl.end() - 1
                brace_close = matching(code, brace_open, "{", "}")
                body = raw[brace_open + 1 : brace_close]
                body_masked = code[brace_open + 1 : brace_close]

                if kind_kw == "struct":
                    fields = []
                    for field_raw in split_top_level(body_masked):
                        # Strip doc/comments from raw field via masked positions, or normalize
                        cleaned = re.sub(r"\bpub\s+", "", field_raw).strip()
                        if not cleaned or ":" not in cleaned:
                            continue
                        fname, ftype = cleaned.split(":", 1)
                        fields.append(
                            StructField(
                                name=fname.strip(),
                                type=parse_rust_type(_normalize_rust_type(ftype)),
                            )
                        )
                    structs.append(StructSpec(name=type_name, fields=tuple(fields)))
                else:
                    variants = split_top_level(body_masked)
                    if is_error or any("=" in v for v in variants):
                        cases = []
                        for v in variants:
                            v = v.strip()
                            if not v or "=" not in v:
                                continue
                            vname, vval = v.split("=", 1)
                            cases.append(EnumCase(name=vname.strip(), value=int(vval.strip(), 0)))
                        target = error_enums if is_error else enums
                        target.append(EnumSpec(name=type_name, cases=tuple(cases)))
                    else:
                        cases_u = []
                        for v in variants:
                            v = v.strip()
                            if not v:
                                continue
                            if "(" in v and v.endswith(")"):
                                idx = v.index("(")
                                cname = v[:idx].strip()
                                inner_types = tuple(
                                    parse_rust_type(_normalize_rust_type(t))
                                    for t in split_top_level(v[idx + 1 : -1])
                                )
                                cases_u.append(UnionCase(name=cname, types=inner_types))
                            else:
                                cases_u.append(UnionCase(name=v, types=()))
                        unions.append(UnionSpec(name=type_name, cases=tuple(cases_u)))
                cursor = brace_close + 1

    return ContractSpec(
        functions=tuple(functions),
        structs=tuple(structs),
        unions=tuple(unions),
        enums=tuple(enums),
        error_enums=tuple(error_enums),
    )


# ---------------------------------------------------------------------------
# Stellar StrKey, .env, README, and Soroban RPC / CLI fetch helpers
# ---------------------------------------------------------------------------

def crc16_xmodem(data: bytes) -> int:
    crc = 0
    for byte in data:
        crc ^= byte << 8
        for _ in range(8):
            if crc & 0x8000:
                crc = ((crc << 1) ^ 0x1021) & 0xFFFF
            else:
                crc = (crc << 1) & 0xFFFF
    return crc


def validate_strkey(address: str, expected_prefix: str | None = None) -> tuple[bool, bytes | str]:
    """Validate a Stellar StrKey (G... or C...) including CRC16-XModem checksum."""
    if not address or len(address) != 56:
        return False, f"expected 56-character StrKey, got length {len(address)}"
    if expected_prefix and not address.startswith(expected_prefix):
        return False, f"expected prefix {expected_prefix!r}, got {address[0]!r}"
    if address[0] not in _STRKEY_VERSION_BYTES:
        return False, f"unsupported StrKey prefix {address[0]!r}"
    try:
        decoded = base64.b32decode(address, casefold=False)
    except Exception as exc:
        return False, f"invalid base32 StrKey: {exc}"
    if len(decoded) != 35:
        return False, f"unexpected decoded StrKey byte length: {len(decoded)}"
    version, payload, checksum = decoded[0], decoded[1:33], decoded[33:35]
    if version != _STRKEY_VERSION_BYTES[address[0]]:
        return False, f"version byte mismatch for prefix {address[0]!r}"
    expected_crc = struct.pack("<H", crc16_xmodem(decoded[:33]))
    if checksum != expected_crc:
        return False, "StrKey CRC16 checksum mismatch"
    return True, payload


def load_env_file(path: Path) -> dict[str, str]:
    """Parse KEY=VALUE pairs safely without shell execution (matches load_env.sh)."""
    env: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[7:].strip()
        if "=" not in line:
            continue
        key, val = line.split("=", 1)
        key = key.strip()
        val = val.strip()
        if not key:
            continue
        if len(val) >= 2 and ((val[0] == val[-1] == '"') or (val[0] == val[-1] == "'")):
            val = val[1:-1]
        elif "#" in val:
            val = val.split("#", 1)[0].strip()
        env[key] = val
    return env


def parse_readme_addresses(readme_path: Path) -> dict[str, str]:
    """Extract canonical testnet addresses from README.md START_DEPLOYED_ADDRESSES table."""
    text = readme_path.read_text(encoding="utf-8")
    begin = "<!-- START_DEPLOYED_ADDRESSES -->"
    end = "<!-- END_DEPLOYED_ADDRESSES -->"
    if begin not in text or end not in text:
        return {}
    block = text.split(begin, 1)[1].split(end, 1)[0]
    mapping = {
        "registry_contract": "registry",
        "invoice_contract": "invoice",
        "escrow_contract": "escrow",
        "pool_contract": "pool",
        "pool_factory_contract": "pool_factory",
    }
    found: dict[str, str] = {}
    for line in block.splitlines():
        m = re.match(r"^\|\s*([a-z0-9_]+)\s*\|\s*`([A-Z2-7]{56})`\s*\|", line.strip())
        if m and m.group(1) in mapping:
            found[mapping[m.group(1)]] = m.group(2)
    return found


def _rpc_call(rpc_url: str, method: str, params: dict, timeout: int = 20) -> dict:
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode("utf-8")
    req = urllib.request.Request(
        rpc_url,
        data=payload,
        headers={"Content-Type": "application/json", "User-Agent": "TrusTrove-Tooling/1.0"},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            body = json.loads(resp.read().decode("utf-8"))
    except urllib.error.URLError as exc:
        raise RuntimeError(f"Soroban RPC request failed ({rpc_url}): {exc}") from exc
    if "error" in body and body["error"]:
        raise RuntimeError(f"Soroban RPC error: {body['error']}")
    return body.get("result", {})


def _decode_contract_code_entry(entry_xdr_b64: str) -> bytes:
    raw = base64.b64decode(entry_xdr_b64)
    # LedgerEntryData union discriminant: 7 = CONTRACT_CODE
    entry_type = struct.unpack(">I", raw[:4])[0]
    if entry_type != 7:
        raise RuntimeError(f"expected CONTRACT_CODE (7), got LedgerEntryType {entry_type}")
    pos = 4
    ext = struct.unpack(">I", raw[pos : pos + 4])[0]
    pos += 4
    if ext == 1:
        # ExtensionPoint (4B) + ContractCodeCostInputs (ExtensionPoint 4B + 10 * uint32)
        pos += 4 + 4 + 40
    pos += 32  # hash
    code_len = struct.unpack(">I", raw[pos : pos + 4])[0]
    pos += 4
    return raw[pos : pos + code_len]


def _extract_wasm_hash_from_instance_entry(entry_xdr_b64: str) -> bytes:
    raw = base64.b64decode(entry_xdr_b64)
    # LedgerEntryData::CONTRACT_DATA(6), ext(0), SCAddress::CONTRACT(1)+32B,
    # SCVal::LEDGER_KEY_CONTRACT_INSTANCE(20), durability(1),
    # SCVal::SCV_CONTRACT_INSTANCE(19), ContractExecutable::WASM(0), hash(32B)
    entry_type = struct.unpack(">I", raw[:4])[0]
    if entry_type != 6:
        raise RuntimeError(f"expected CONTRACT_DATA (6), got LedgerEntryType {entry_type}")
    exec_type = struct.unpack(">I", raw[56:60])[0]
    if exec_type != 0:
        raise RuntimeError(f"contract executable is not WASM (type={exec_type})")
    return raw[60:92]


def fetch_contract_wasm_rpc(contract_id: str, rpc_url: str, timeout: int = 20) -> tuple[bytes, str]:
    """Fetch a deployed contract's WASM bytes and SHA-256 hash via Soroban RPC."""
    ok, payload = validate_strkey(contract_id, expected_prefix="C")
    if not ok:
        raise ValueError(f"invalid contract ID {contract_id!r}: {payload}")
    assert isinstance(payload, bytes)
    instance_key = base64.b64encode(
        struct.pack(">II", 6, 1) + payload + struct.pack(">II", 20, 1)
    ).decode("ascii")
    res = _rpc_call(rpc_url, "getLedgerEntries", {"keys": [instance_key]}, timeout=timeout)
    entries = res.get("entries") or []
    if not entries:
        raise RuntimeError(f"contract instance not found on-chain: {contract_id}")
    wasm_hash = _extract_wasm_hash_from_instance_entry(entries[0]["xdr"])
    code_key = base64.b64encode(struct.pack(">I", 7) + wasm_hash).decode("ascii")
    code_res = _rpc_call(rpc_url, "getLedgerEntries", {"keys": [code_key]}, timeout=timeout)
    code_entries = code_res.get("entries") or []
    if not code_entries:
        raise RuntimeError(f"contract WASM code not found on-chain for hash {wasm_hash.hex()}")
    wasm_bytes = _decode_contract_code_entry(code_entries[0]["xdr"])
    return wasm_bytes, hashlib.sha256(wasm_bytes).hexdigest()


def find_stellar_cli() -> str | None:
    """Locate the stellar CLI binary if installed."""
    found = shutil.which("stellar")
    if found:
        return found
    wsl_path = Path("/mnt/c/Program Files (x86)/Stellar CLI/stellar.exe")
    if wsl_path.is_file():
        return str(wsl_path)
    return None


def fetch_contract_wasm_stellar(
    contract_id: str,
    network: str,
    stellar_bin: str | None = None,
) -> tuple[bytes, str]:
    """Fetch a deployed contract's WASM bytes using `stellar contract fetch`."""
    ok, err = validate_strkey(contract_id, expected_prefix="C")
    if not ok:
        raise ValueError(f"invalid contract ID {contract_id!r}: {err}")
    cli = stellar_bin or find_stellar_cli()
    if not cli:
        raise RuntimeError("stellar CLI not found on PATH")
    with tempfile.TemporaryDirectory(prefix="trusttrove-fetch-") as tmpdir:
        out_path = Path(tmpdir) / "contract.wasm"
        proc = subprocess.run(
            [cli, "contract", "fetch", "--id", contract_id, "--network", network, "--out-file", str(out_path)],
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            raise RuntimeError(
                f"stellar contract fetch failed for {contract_id}: {proc.stderr.strip() or proc.stdout.strip()}"
            )
        wasm_bytes = out_path.read_bytes()
    return wasm_bytes, hashlib.sha256(wasm_bytes).hexdigest()


def read_local_specs(
    wasm_dir: Path | None = None,
    from_source: bool = False,
    root: Path = ROOT,
    contracts: Iterable[tuple[str, str, str, str]] = CONTRACTS,
) -> dict[str, ContractSpec]:
    """Load canonical ContractSpec for each contract from compiled WASM or Rust source."""
    specs: dict[str, ContractSpec] = {}
    for key, pascal_name, wasm_name, crate_rel in contracts:
        if from_source:
            raw_spec = parse_rust_contract_spec(root / crate_rel)
        else:
            if wasm_dir is None:
                raise ValueError("wasm_dir is required unless from_source is True")
            wasm_path = wasm_dir / wasm_name
            if not wasm_path.is_file():
                raise FileNotFoundError(f"compiled WASM artifact not found: {wasm_path}")
            raw_spec = parse_wasm_spec(wasm_path.read_bytes())
        specs[key] = raw_spec.canonical(contract_pascal_name=pascal_name)
    return specs
