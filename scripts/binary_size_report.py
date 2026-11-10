#!/usr/bin/env python3
"""Explain where the bytes of a Rust ELF binary come from.

The report needs an unstripped binary (the symbol table, not debug info) and
the binutils tools `size` and `nm`. It prints:

  * the size of each loaded section, and what a packaging strip removes;
  * bytes per crate, attributing `<T as Trait>::f` and std generics to the
    first non-std crate named in the symbol;
  * functions that exist several times under the same path. rustc gives every
    crate that instantiates a generic or drives an upstream `async fn` its own
    copy, so this shows how much of the binary is repeated code.

Examples:
    # Build with symbols but without debug info, then report.
    cd codex-rs
    CARGO_PROFILE_RELEASE_STRIP=none cargo build --release -p codex-cli --bin codex
    ../scripts/binary_size_report.py target/release/codex

    # Compare two builds crate by crate.
    ../scripts/binary_size_report.py new/codex --json new.json
    ../scripts/binary_size_report.py old/codex --json old.json --compare new.json
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import re
import subprocess
import sys

STD_CRATES = {
    "core",
    "alloc",
    "std",
    "compiler_builtins",
    "panic_unwind",
    "std_detect",
    "addr2line",
    "gimli",
    "miniz_oxide",
    "object",
    "rustc_demangle",
}
NOT_LOADED = (".debug", ".symtab", ".strtab", ".comment", ".shstrtab")
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
# The first segment of a path: at the start, or after `<`, `(`, `&`, `,` and the like.
PATH_HEAD = re.compile(r"(?:^|[<(\[\s,&*])([a-z_][a-z0-9_]*)::")
HASH_SUFFIX = re.compile(r"::h[0-9a-f]{16}$")


def section_sizes(binary: str) -> dict[str, int]:
    output = subprocess.run(
        ["size", "-A", "-d", binary], capture_output=True, text=True, check=True
    ).stdout
    sections = {}
    for line in output.splitlines():
        parts = line.split()
        if len(parts) == 3 and parts[1].isdigit():
            sections[parts[0]] = int(parts[1])
    return sections


def crate_of(name: str) -> str:
    """The crate a demangled Rust symbol should be charged to."""
    if name.startswith("anon."):
        return "(anonymous static data)"
    if "::" not in name and not name.startswith("<"):
        return "(C and assembly)"
    # `<T as Trait>::f` and std generics carry the interesting crate inside.
    stripped = name.lstrip("<&*( ")
    head = IDENT.match(stripped)
    first = head.group(0) if head else ""
    if (
        first
        and first not in STD_CRATES
        and first[0].islower()
        and stripped.startswith(first + "::")
    ):
        return first
    for candidate in PATH_HEAD.findall(name):
        if candidate not in STD_CRATES:
            return candidate
    return "(std)" if first in STD_CRATES else first or "?"


def read_symbols(binary: str) -> list[tuple[int, str, str]]:
    output = subprocess.run(
        ["nm", "-S", "-C", "--defined-only", binary],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    symbols = []
    for line in output.splitlines():
        parts = line.split(None, 3)
        if len(parts) < 4 or len(parts[1]) != 16:
            continue
        try:
            size = int(parts[1], 16)
        except ValueError:
            continue
        if size:
            symbols.append((size, parts[2], parts[3].strip()))
    return symbols


def megabytes(value: float) -> str:
    return f"{value / 1e6:8.2f} MB"


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("binary")
    parser.add_argument("--top", type=int, default=30, help="rows per table")
    parser.add_argument("--json", help="write the per-crate totals to this file")
    parser.add_argument(
        "--compare", help="per-crate totals of another build, from --json"
    )
    args = parser.parse_args()

    sections = section_sizes(args.binary)
    loaded = {k: v for k, v in sections.items() if not k.startswith(NOT_LOADED)}
    file_size = os.path.getsize(args.binary)
    # `size` does not list the symbol and string tables, so take the remainder.
    print(f"file on disk        {megabytes(file_size)}")
    print(f"symbols and debug   {megabytes(file_size - sum(loaded.values()))}")
    print(f"loaded sections     {megabytes(sum(loaded.values()))}")
    for name, size in sorted(loaded.items(), key=lambda item: -item[1])[:9]:
        print(f"    {name:20}{megabytes(size)}")

    symbols = read_symbols(args.binary)
    if not symbols:
        print("\nno symbol table: build with CARGO_PROFILE_RELEASE_STRIP=none")
        return 1
    by_crate: collections.Counter[str] = collections.Counter()
    by_path: dict[str, list[int]] = collections.defaultdict(list)
    for size, kind, name in symbols:
        by_crate[crate_of(name)] += size
        if kind in "TtWw":
            by_path[HASH_SUFFIX.sub("", name)].append(size)
    total = sum(by_crate.values())
    workspace = sum(v for k, v in by_crate.items() if k.startswith("codex"))
    print(f"\nsymbol bytes        {megabytes(total)}")
    print(
        f"  workspace crates  {megabytes(workspace)}  ({100 * workspace / total:.0f}%)"
    )

    other = {}
    if args.compare:
        with open(args.compare, encoding="utf-8") as handle:
            other = json.load(handle)["by_crate"]
    print(f"\ntop {args.top} crates:")
    for crate, size in by_crate.most_common(args.top):
        line = f"  {megabytes(size)} {100 * size / total:5.1f}%  {crate}"
        if other:
            line += f"   (other build: {megabytes(other.get(crate, 0)).strip()})"
        print(line)

    function_bytes = sum(sum(sizes) for sizes in by_path.values())
    repeated = [
        (sum(sizes) - max(sizes), len(sizes), path)
        for path, sizes in by_path.items()
        if len(sizes) > 1
    ]
    extra = sum(row[0] for row in repeated)
    print(
        f"\nsame-path function copies beyond the largest: {megabytes(extra).strip()}"
        f" of {megabytes(function_bytes).strip()} of functions"
        " (distinct type arguments share a path, so this is an upper bound)"
    )
    for size, copies, path in sorted(repeated, reverse=True)[: args.top]:
        print(f"  {size / 1e3:8.1f} KB x{copies:<5} {path[:120]}")

    if args.json:
        with open(args.json, "w", encoding="utf-8") as handle:
            json.dump({"sections": sections, "by_crate": dict(by_crate)}, handle)
    return 0


if __name__ == "__main__":
    sys.exit(main())
