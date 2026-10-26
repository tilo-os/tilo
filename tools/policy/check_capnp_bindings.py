#!/usr/bin/env python3
"""Keep checked-in Cap'n Proto bindings in step with their schemas.

Crates check their generated bindings in under ``src/generated/`` so building
them never needs the ``capnp`` compiler. ``capnp_bindings.toml`` lists each
such crate with the digests of its schemas and of its bindings as
``tools/regenerate-capnp.sh`` last wrote them; a schema edited without
regenerating, or bindings edited by hand, fail here.

``--record CRATE_DIR`` rewrites that crate's digests after a regeneration.
"""

from __future__ import annotations

import hashlib
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "tools/policy/capnp_bindings.toml"


def generated(schema: str) -> str:
    return f"src/generated/{Path(schema).stem.replace('-', '_')}_capnp.rs"


def digest(crate: Path, paths: list[str]) -> str:
    hasher = hashlib.sha256()
    for path in paths:
        hasher.update((crate / path).read_bytes())
    return hasher.hexdigest()


def digests(entry: dict) -> tuple[str, str]:
    crate = ROOT / entry["path"]
    schemas = entry["schemas"]
    return digest(crate, schemas), digest(crate, [generated(s) for s in schemas])


def regenerate_command(entry: dict) -> str:
    return f"tools/regenerate-capnp.sh {entry['path']}"


def load() -> list[dict]:
    return tomllib.loads(MANIFEST.read_text())["crate"]


def write(entries: list[dict]) -> None:
    header = MANIFEST.read_text().split("[[crate]]", 1)[0]
    blocks = []
    for entry in entries:
        schemas = ", ".join(f'"{schema}"' for schema in entry["schemas"])
        blocks.append(
            "[[crate]]\n"
            f'path = "{entry["path"]}"\n'
            f'src_prefix = "{entry["src_prefix"]}"\n'
            f"schemas = [{schemas}]\n"
            f'schema_sha256 = "{entry["schema_sha256"]}"\n'
            f'bindings_sha256 = "{entry["bindings_sha256"]}"\n'
        )
    MANIFEST.write_text(header + "\n".join(blocks))


def record(crate_dir: str) -> int:
    entries = load()
    wanted = (ROOT / crate_dir).resolve()
    for entry in entries:
        if (ROOT / entry["path"]).resolve() == wanted:
            entry["schema_sha256"], entry["bindings_sha256"] = digests(entry)
            write(entries)
            return 0
    print(f"{MANIFEST.relative_to(ROOT)} lists no crate at {crate_dir}", file=sys.stderr)
    return 1


def check() -> int:
    failures = []
    for entry in load():
        schema, bindings = digests(entry)
        if schema != entry["schema_sha256"]:
            stale = "its Cap'n Proto schema changed without regenerating its bindings"
        elif bindings != entry["bindings_sha256"]:
            stale = "its checked-in Cap'n Proto bindings were edited by hand"
        else:
            continue
        # The crate's findings name its manifest, the file that declares it.
        failures.append(f"{entry['path']}/Cargo.toml: {stale}; run `{regenerate_command(entry)}`")
    for failure in failures:
        print(failure)
    return 1 if failures else 0


def main(argv: list[str]) -> int:
    if len(argv) == 2 and argv[0] == "--record":
        return record(argv[1])
    if argv:
        print(f"usage: {sys.argv[0]} [--record CRATE_DIR]", file=sys.stderr)
        return 2
    return check()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
