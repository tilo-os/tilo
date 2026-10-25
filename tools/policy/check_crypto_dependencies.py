#!/usr/bin/env python3
"""Keep raw AEAD, KEM and password-KDF crates behind `tilo-seal`.

Sealing constructions (key agreement, AEAD framing, nonce handling, password
stretching) live in one reviewed crate, ``crates/system/tilo-seal``. Every
other workspace crate seals through its typed API. This check fails when a
workspace manifest names one of the raw primitive crates directly, in any
dependency table (normal, dev, build, or target-specific), including through
``package = "..."`` renames and ``workspace = true`` inheritance.

The cargo-hakari package (``hakari-package`` in ``.config/hakari.toml``) and
workspace-unify (tools/ci/workspace_unify.py) name dependencies only to unify
their features, and a crate can use only its own direct dependencies, so they
are exempt while their sources contain no code.
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
    # Apple's iCloud sign-in is SRP-6a over a PBKDF2 of the password, fixed
    # by the protocol; tilo-crypto's `srp_apple` implements that exchange
    # and seals nothing with it.
    "tilo-crypto": frozenset({"pbkdf2"}),
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


def hakari_package(root: Path) -> str | None:
    config = root / ".config/hakari.toml"
    if not config.is_file():
        return None
    name = load_manifest(config).get("hakari-package")
    return name if isinstance(name, str) else None


def code_free(crate: Path) -> bool:
    """True when every Rust source is comments, blank lines or an empty build script."""
    for source in crate.rglob("*.rs"):
        code = [
            line.strip()
            for line in source.read_text(encoding="utf-8").splitlines()
            if line.strip() and not line.strip().startswith("//")
        ]
        if code not in ([], ["fn main() {}"]):
            return False
    return True


# tools/ci/workspace_unify.py's package: every library the workspace builds, to unify features.
UNIFY_PACKAGE = "workspace-unify"


def violations(root: Path = ROOT) -> list[str]:
    renames = workspace_renames(root)
    feature_crate = hakari_package(root)
    errors: list[str] = []
    for member in workspace_members(root):
        manifest_path = root / member / "Cargo.toml"
        if not manifest_path.is_file():
            continue
        manifest = load_manifest(manifest_path)
        package = manifest.get("package", {}).get("name", member)
        allowed = ALLOWED.get(package, frozenset())
        if package in (feature_crate, UNIFY_PACKAGE) and code_free(root / member):
            allowed = FORBIDDEN
        for table_name, table in dependency_tables(manifest):
            for key, dependency in named_packages(table, renames):
                if dependency in FORBIDDEN and dependency not in allowed:
                    shown = dependency if key == dependency else f"{key} (package {dependency})"
                    errors.append(
                        f"{member}/Cargo.toml: [{table_name}] depends on {shown}; "
                        "seal through tilo-seal instead"
                    )
    return errors


def main() -> int:
    errors = violations()
    for error in errors:
        print(error)
    if errors:
        return 1
    print("raw AEAD/KEM/KDF crates are only used by tilo-seal", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
