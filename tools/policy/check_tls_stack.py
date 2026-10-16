#!/usr/bin/env python3
"""Keep rustls the only TLS stack: no OpenSSL anywhere in the dependency graph.

TLS in Tilo is rustls on ring, reached through ``tilo_net::http`` for HTTP.
OpenSSL would be a second certificate-verification path to review, a C library
to patch on every image, and a runtime dependency an app could not assume on
macOS, Windows or the web. So the resolved graph in ``Cargo.lock`` may not
contain, directly or transitively:

* ``openssl`` / ``openssl-sys``: the OpenSSL bindings;
* ``native-tls``: the platform-TLS wrapper, which is OpenSSL on Linux.

A dependency that pulls one in by default usually has a ``rustls`` feature;
select it and turn its default features off.
"""

from __future__ import annotations

from pathlib import Path
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[2]
FORBIDDEN = {"openssl", "openssl-sys", "native-tls"}


def check(root: Path = ROOT) -> list[str]:
    """Return a line for each forbidden package and each package that depends on one."""
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    found = []
    for package in lock.get("package", []):
        name = package["name"]
        if name in FORBIDDEN:
            found.append(f"Cargo.lock contains {name} {package['version']}")
        for entry in package.get("dependencies", []):
            dependency = entry.split()[0]
            if dependency in FORBIDDEN:
                found.append(f"{name} {package['version']} depends on {dependency}")
    return sorted(found)


def main() -> int:
    found = check()
    if found:
        print("rustls is the only TLS stack; OpenSSL is not allowed (docs/policies.md):")
        for line in found:
            print(f"  {line}")
        return 1
    print("No OpenSSL in the dependency graph.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
