#!/usr/bin/env python3
"""Every path a system unit's sandbox names exists by the time the unit starts.

systemd builds a unit's mount namespace before it runs anything, and a
ReadWritePaths=, InaccessiblePaths= or Bind*Paths= entry whose path is missing
fails that step: the unit dies with status 226/NAMESPACE before its own code
could have created the directory or decided there was nothing to do. A path
prefixed with "-" is skipped instead, so it is never a problem.

tilo-rollback-confirm.service failed on every boot of a machine without a
rollback token this way: it named /run/tilo-rollback and /var/lib/tilo/rollback,
and nothing created either until enrollment.

ReadOnlyPaths= is not checked: under ProtectSystem=strict a read-only entry
below the read-only root is dropped as a no-op before anything is mounted.

A path is accounted for when tmpfiles.d creates it (or something above it that
the unit can reach through it), the unit's own *Directory= creates it, it ships
in the image tree under os/, or PROVIDED names who else creates it.
"""

from __future__ import annotations

import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
UNITS = REPO / "os/usr/lib/systemd/system"
TMPFILES = REPO / "os/usr/lib/tmpfiles.d"

CHECKED = (
    "ReadWritePaths",
    "InaccessiblePaths",
    "ExecPaths",
    "NoExecPaths",
    "BindPaths",
    "BindReadOnlyPaths",
)

DIRECTORY_SETTINGS = {
    "StateDirectory": "/var/lib",
    "RuntimeDirectory": "/run",
    "CacheDirectory": "/var/cache",
    "LogsDirectory": "/var/log",
    "ConfigurationDirectory": "/etc",
}

# Created by something other than this repository's tmpfiles, with the reason.
PROVIDED = {
    "/boot": "the boot partition's mount point, part of every bootc deployment",
    "/sysroot": "the physical root's mount point, part of every bootc deployment",
    "/ostree": "bootc's deployment root",
    "/var/lib/containers": "containers-common's tmpfiles",
    "/var/lib/bootc": "bootc's own state, present on every deployed system",
    "/sys": "the kernel's sysfs",
    "/etc/cups": "the cups package",
    "/var/spool/cups": "the cups package's tmpfiles",
    "/var/cache/cups": "the cups package's tmpfiles",
    "/var/log/cups": "the cups package's tmpfiles",
    "/run/cups": "the cups package's tmpfiles",
    "/run/tilo-installer": "tilo-installer-setup.service, which every installer unit naming it is ordered after",
}


def unit_files() -> list[Path]:
    files = sorted(UNITS.glob("*.service"))
    files += sorted(UNITS.glob("*.service.d/*.conf"))
    return files


def settings(path: Path) -> list[tuple[str, str]]:
    out = []
    for raw in path.read_text().splitlines():
        line = raw.strip()
        if not line or line[0] in "#;[" or "=" not in line:
            continue
        key, _, value = line.partition("=")
        out.append((key.strip(), value.strip()))
    return out


def tmpfiles_paths() -> set[str]:
    paths = set()
    for conf in sorted(TMPFILES.glob("*.conf")):
        for raw in conf.read_text().splitlines():
            fields = raw.split()
            if len(fields) >= 2 and not fields[0].startswith("#"):
                paths.add(fields[1].rstrip("/"))
    return paths


def under(path: str, root: str) -> bool:
    return path == root or path.startswith(root.rstrip("/") + "/")


def accounted(path: str, created: set[str], own: list[str]) -> bool:
    if path in created or any(under(path, root) for root in own):
        return True
    if any(under(path, root) for root in PROVIDED):
        return True
    return (REPO / "os" / path.lstrip("/")).exists()


def main() -> int:
    created = tmpfiles_paths()
    failures = []
    for unit in unit_files():
        values = settings(unit)
        own = [
            f"{DIRECTORY_SETTINGS[key]}/{name}"
            for key, value in values
            if key in DIRECTORY_SETTINGS
            for name in value.split()
        ]
        for key, value in values:
            if key not in CHECKED:
                continue
            for token in value.split():
                if token.startswith("-"):
                    continue
                path = token.lstrip("+").split(":", 1)[0]
                # Specifiers (%i, %S, …) resolve per instance; the caller that
                # starts the instance owns the directory.
                if "%" in path:
                    continue
                if not accounted(path, created, own):
                    failures.append(f"{unit.relative_to(REPO)}: {key}= names {path}, which nothing creates")
    for failure in failures:
        print(f"FAIL  {failure}", file=sys.stderr)
    if failures:
        print(
            "systemd fails such a unit with 226/NAMESPACE before it runs. Create the path in "
            "os/usr/lib/tmpfiles.d, give the unit a *Directory= for it, or prefix it with '-'.",
            file=sys.stderr,
        )
        return 1
    print(f"ok    every sandbox path in {len(unit_files())} system units exists when the unit starts")
    return 0


if __name__ == "__main__":
    sys.exit(main())
