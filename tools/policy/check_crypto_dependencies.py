#!/usr/bin/env python3
"""Keep raw AEAD, KEM and password-KDF crates behind `tilo-seal`.

Sealing constructions (key agreement, AEAD framing, nonce handling, password
stretching) live in one reviewed crate, ``crates/system/tilo-seal``. Every
other workspace crate seals through its typed API. This check fails when a
workspace manifest names one of the raw primitive crates directly, in any
dependency table (normal, dev, build, or target-specific), including through
``package = "..."`` renames and ``workspace = true`` inheritance.
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]

# Raw symmetric AEADs, key agreement / KEMs, HPKE implementations and
# password KDFs. Protocol libraries that own their whole construction (snow's
# Noise, openmls's MLS) are not listed: they are adapters, not primitives.
FORBIDDEN = frozenset(
    {
        "aead",
        "aes",
        "aes-gcm",
        "aes-gcm-siv",
        "aes-siv",
        "argon2",
        "balloon-hash",
        "chacha20",
        "chacha20poly1305",
        "crypto_box",
        "crypto_secretbox",
        "curve25519-dalek",
        "hpke",
        "hpke-rs",
        "hpke-rs-crypto",
        "hpke-rs-libcrux",
        "hpke-rs-rust-crypto",
        "libcrux-chacha20poly1305",
        "libcrux-ecdh",
        "libcrux-hpke",
        "libcrux-kem",
        "orion",
        "pbkdf2",
        "poly1305",
        "salsa20",
        "scrypt",
        "sodiumoxide",
        "x25519-dalek",
        "xsalsa20poly1305",
    }
)

# Package name -> forbidden crates it may name. Keep this list short and
# justified: every entry is a crate whose job is the construction itself.
ALLOWED = {
    "tilo-seal": FORBIDDEN,
}

# Crates whose formats are still being moved onto tilo-seal. Each entry is
# removed by the change that migrates that crate; nothing may be added.
MIGRATING = {
    "channel-crypto": frozenset({"chacha20poly1305"}),
    "passwords": frozenset({"argon2", "chacha20poly1305"}),
    "runtime-identity": frozenset({"argon2", "chacha20poly1305"}),
    "shell": frozenset({"chacha20poly1305", "x25519-dalek"}),
    "tilo-relay": frozenset({"chacha20poly1305", "x25519-dalek"}),
    "tilo-ui": frozenset({"chacha20poly1305"}),
}

DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")


def load_manifest(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def workspace_members(root: Path) -> list[str]:
    workspace = load_manifest(root / "Cargo.toml").get("workspace", {})
    members = workspace.get("members", [])
    if not isinstance(members, list):
        raise ValueError("[workspace].members must be a list")
    return [member for member in members if isinstance(member, str)]


def workspace_renames(root: Path) -> dict[str, str]:
    """Map `[workspace.dependencies]` keys to the package they name."""
    table = load_manifest(root / "Cargo.toml").get("workspace", {}).get("dependencies", {})
    renames: dict[str, str] = {}
    for key, spec in table.items():
        if isinstance(spec, dict) and isinstance(spec.get("package"), str):
            renames[key] = spec["package"]
        else:
            renames[key] = key
    return renames


def dependency_tables(manifest: dict) -> list[tuple[str, dict]]:
    tables: list[tuple[str, dict]] = []
    for name in DEPENDENCY_TABLES:
        table = manifest.get(name)
        if isinstance(table, dict):
            tables.append((name, table))
    targets = manifest.get("target", {})
    if isinstance(targets, dict):
        for target, body in targets.items():
            if not isinstance(body, dict):
                continue
            for name in DEPENDENCY_TABLES:
                table = body.get(name)
                if isinstance(table, dict):
                    tables.append((f"target.{target}.{name}", table))
    return tables


def named_packages(table: dict, renames: dict[str, str]) -> list[tuple[str, str]]:
    """Yield `(key, package)` for every dependency in `table`."""
    out: list[tuple[str, str]] = []
    for key, spec in table.items():
        package = key
        if isinstance(spec, dict):
            if isinstance(spec.get("package"), str):
                package = spec["package"]
            elif spec.get("workspace") is True:
                package = renames.get(key, key)
        out.append((key, package))
    return out


def violations(root: Path = ROOT) -> list[str]:
    renames = workspace_renames(root)
    errors: list[str] = []
    for member in workspace_members(root):
        manifest_path = root / member / "Cargo.toml"
        if not manifest_path.is_file():
            continue
        manifest = load_manifest(manifest_path)
        package = manifest.get("package", {}).get("name", member)
        allowed = ALLOWED.get(package, frozenset()) | MIGRATING.get(package, frozenset())
        for table_name, table in dependency_tables(manifest):
            for key, dependency in named_packages(table, renames):
                if dependency in FORBIDDEN and dependency not in allowed:
                    shown = dependency if key == dependency else f"{key} (package {dependency})"
                    errors.append(
                        f"{member}/Cargo.toml [{table_name}] depends on {shown}; "
                        "seal through tilo-seal instead"
                    )
    return errors


def main() -> int:
    errors = violations()
    for error in errors:
        print(error, file=sys.stderr)
    if errors:
        return 1
    print("raw AEAD/KEM/KDF crates are only used by tilo-seal")
    return 0


if __name__ == "__main__":
    sys.exit(main())
