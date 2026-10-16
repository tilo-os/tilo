#!/usr/bin/env python3
"""Keep telemetry, analytics and crash-upload clients out of the dependency graph.

docs/architecture/security.md ("What leaves the device by default") states
that Tilo's own components carry no telemetry, analytics or crash-upload
client: panics are written to ``~/.cache/tilo/crashes`` and feedback leaves
only as a report the person approved, to a room they configured. These are
the crates that statement would stop being true with, so the resolved graph
in ``Cargo.lock`` may not contain any of them, directly or transitively.

Local tracing (the ``tracing`` crates, ``opentelemetry`` without an
exporter) is not on the list: it never leaves the device by itself.
"""

from __future__ import annotations

from pathlib import Path
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[2]
FORBIDDEN = {
    # Error and crash reporting services.
    "sentry",
    "sentry-core",
    "bugsnag",
    # Minidump capture, which exists to upload a crash elsewhere.
    "crash-handler",
    "minidumper",
    # Product analytics.
    "posthog-rs",
    "segment",
    # Telemetry exporters that send spans and metrics off the device.
    "opentelemetry-otlp",
    "opentelemetry-http",
    "opentelemetry-datadog",
    "opentelemetry-zipkin",
}


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
        print(
            "Tilo carries no telemetry, analytics or crash-upload client "
            "(docs/architecture/security.md, docs/policies.md):"
        )
        for line in found:
            print(f"  {line}")
        return 1
    print("No telemetry, analytics or crash-upload client in the dependency graph.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
