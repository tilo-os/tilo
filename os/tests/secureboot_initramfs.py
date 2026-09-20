#!/usr/bin/env python3
"""Write the exact initramfs list used by secureboot_vm_probe.sh."""

from __future__ import annotations

import argparse
import os
from pathlib import Path, PurePosixPath
import re
import resource
import shutil
import subprocess
import sys
import tempfile


LDD_OUTPUT_LIMIT = 64 * 1024
MAX_LDD_LINES = 128
MAX_LDD_LINE_BYTES = 4096
MAX_RUNTIME_FILES = 128
LDD_TIMEOUT_SECONDS = 10
ADDRESS = r"(?: \(0x[0-9a-fA-F]+\))?"
ARROW_LINE = re.compile(rf"(?P<identity>[^/\s]+) => (?P<path>/\S+){ADDRESS}")
DIRECT_LINE = re.compile(rf"(?P<path>/\S+){ADDRESS}")
VDSO_LINE = re.compile(r"linux-vdso\.so\.1 \(0x[0-9a-fA-F]+\)")


def valid_archive_path(value: str, label: str) -> str:
    path = PurePosixPath(value)
    if (
        not value.startswith("/")
        or value.startswith("//")
        or str(path) != value
        or ".." in path.parts
        or any(character.isspace() for character in value)
    ):
        raise ValueError(f"invalid absolute {label} path: {value!r}")
    return value


def valid_source(value: str, label: str) -> str:
    if (
        not os.path.isabs(value)
        or any(character.isspace() for character in value)
        or not Path(value).is_file()
    ):
        raise ValueError(f"missing or invalid absolute {label} source: {value!r}")
    return value


def ancestors(destination: str) -> list[str]:
    path = PurePosixPath(destination).parent
    result = []
    while str(path) != "/":
        result.append(str(path))
        path = path.parent
    return result


def parse_ldd(output: bytes, label: str) -> list[tuple[str, str, str]]:
    raw_lines = [line.strip() for line in output.splitlines() if line.strip()]
    if len(raw_lines) > MAX_LDD_LINES:
        raise ValueError(f"{label} ldd output exceeds {MAX_LDD_LINES} lines")
    mappings = []
    loaders = 0
    libraries = 0
    vdso = 0
    for raw_line in raw_lines:
        if len(raw_line) > MAX_LDD_LINE_BYTES:
            raise ValueError(
                f"{label} ldd line exceeds {MAX_LDD_LINE_BYTES} bytes"
            )
        line = raw_line.decode("utf-8")
        if VDSO_LINE.fullmatch(line):
            vdso += 1
            if vdso > 1:
                raise ValueError(f"{label} ldd output has duplicate VDSO entries")
            continue
        arrow = ARROW_LINE.fullmatch(line)
        if arrow:
            identity = arrow.group("identity")
            destination = valid_archive_path(
                arrow.group("path"), f"{label} dependency"
            )
            mappings.append((identity, destination, destination))
            libraries += 1
            continue
        direct = DIRECT_LINE.fullmatch(line)
        if direct:
            destination = valid_archive_path(
                direct.group("path"), f"{label} loader"
            )
            if not PurePosixPath(destination).name.startswith("ld-linux"):
                raise ValueError(f"unexpected direct {label} ldd entry: {line!r}")
            mappings.append((destination, destination, destination))
            loaders += 1
            continue
        if "=> not found" in line:
            raise ValueError(f"unresolved {label} dependency: {line!r}")
        raise ValueError(f"malformed {label} ldd output: {line!r}")
    if libraries == 0:
        raise ValueError(f"{label} ldd output resolved no shared libraries")
    if loaders != 1:
        raise ValueError(
            f"{label} ldd output must contain exactly one loader, found {loaders}"
        )
    return mappings


def run_ldd(ldd: str, binary: str, label: str) -> bytes:
    environment = os.environ.copy()
    for name in list(environment):
        if name.startswith("LD_"):
            environment.pop(name)
    environment["LC_ALL"] = "C"
    environment["LANG"] = "C"

    def limit_output() -> None:
        resource.setrlimit(
            resource.RLIMIT_FSIZE, (LDD_OUTPUT_LIMIT, LDD_OUTPUT_LIMIT)
        )

    with tempfile.TemporaryDirectory(prefix="tilo-secureboot-ldd-") as directory:
        stdout_path = Path(directory) / "stdout"
        stderr_path = Path(directory) / "stderr"
        try:
            with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
                result = subprocess.run(
                    [ldd, binary],
                    env=environment,
                    stdout=stdout,
                    stderr=stderr,
                    preexec_fn=limit_output,
                    timeout=LDD_TIMEOUT_SECONDS,
                )
        except subprocess.TimeoutExpired as error:
            raise ValueError(
                f"{label} ldd timed out after {LDD_TIMEOUT_SECONDS} seconds"
            ) from error
        stdout = stdout_path.read_bytes()
        stderr = stderr_path.read_bytes()
    if result.returncode != 0:
        detail = stderr.decode("utf-8", errors="replace").strip()
        raise ValueError(
            f"{label} ldd failed with status {result.returncode}: "
            f"{detail or 'no diagnostic'}"
        )
    if len(stdout) > LDD_OUTPUT_LIMIT or len(stderr) > LDD_OUTPUT_LIMIT:
        raise ValueError(f"{label} ldd output exceeded {LDD_OUTPUT_LIMIT} bytes")
    if stderr:
        raise ValueError(
            f"{label} ldd wrote stderr: "
            f"{stderr.decode('utf-8', errors='replace').strip()!r}"
        )
    return stdout


def runtime_dependencies(init: str, crypto: str) -> list[tuple[str, str]]:
    ldd = shutil.which("ldd")
    if ldd is None:
        raise ValueError("required ldd utility not found on PATH")
    discovered = []
    for label, binary in (("init", init), ("tilo-crypto", crypto)):
        binary = valid_source(binary, label)
        discovered.extend(parse_ldd(run_ldd(ldd, binary, label), label))

    identities: dict[str, tuple[str, str]] = {}
    destinations: dict[str, tuple[str, str]] = {}
    unique: dict[tuple[str, str, str], None] = {}
    for identity, destination, source in discovered:
        prior_identity = identities.get(identity)
        if prior_identity is not None and prior_identity != (destination, source):
            raise ValueError(
                f"ambiguous runtime dependency {identity!r}: "
                f"{prior_identity[0]!r} and {destination!r}"
            )
        prior_destination = destinations.get(destination)
        if prior_destination is not None and prior_destination != (identity, source):
            raise ValueError(
                f"runtime dependency destination collision: {destination}"
            )
        identities[identity] = (destination, source)
        destinations[destination] = (identity, source)
        unique[(identity, destination, source)] = None
    if len(unique) > MAX_RUNTIME_FILES:
        raise ValueError(
            f"runtime closure exceeds {MAX_RUNTIME_FILES} unique files"
        )
    mappings = [
        (destination, source) for _, destination, source in unique
    ]
    return sorted(mappings)


def write_list(
    output: Path,
    init: str,
    crypto: str,
    material_dir: str,
    dependencies: list[tuple[str, str]],
) -> None:
    material = Path(material_dir)
    base_files = [
        ("/init", init, "0755"),
        ("/tilo-crypto", crypto, "0755"),
        ("/PK.esl", str(material / "PK.esl"), "0600"),
        ("/KEK.esl", str(material / "KEK.esl"), "0600"),
        ("/db.esl", str(material / "db.esl"), "0600"),
        ("/PK.key", str(material / "PK.key"), "0600"),
        ("/PK.crt", str(material / "PK.crt"), "0600"),
    ]
    files: list[tuple[str, str, str]] = []
    destinations: set[str] = set()
    for destination, source, mode in base_files + [
        (destination, source, "0755") for destination, source in dependencies
    ]:
        destination = valid_archive_path(destination, "archive destination")
        if destination in destinations:
            raise ValueError(f"duplicate initramfs destination: {destination}")
        destinations.add(destination)
        files.append((destination, source, mode))

    directories = {"/sys"}
    fixed_conflicts = destinations & directories
    if fixed_conflicts:
        raise ValueError(
            f"initramfs path is both a file and directory: {min(fixed_conflicts)}"
        )
    for destination in destinations:
        for directory in ancestors(destination):
            if directory in destinations:
                raise ValueError(
                    f"initramfs path is both a file and directory: {directory}"
                )
            directories.add(directory)
    for destination, source, _ in files:
        valid_source(source, destination)
    ordered_directories = sorted(
        directories, key=lambda value: (len(PurePosixPath(value).parts), value)
    )
    lines = [f"dir {directory} 0755 0 0" for directory in ordered_directories]
    lines.extend(
        f"file {destination} {source} {mode} 0 0"
        for destination, source, mode in files
    )

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w", dir=output.parent, prefix=output.name + ".", delete=False
    ) as stream:
        temporary = Path(stream.name)
        stream.write("\n".join(lines) + "\n")
    temporary.replace(output)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("--init", required=True)
    parser.add_argument("--crypto", required=True)
    parser.add_argument("--material-dir", required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        dependencies = runtime_dependencies(args.init, args.crypto)
        write_list(
            args.output,
            args.init,
            args.crypto,
            args.material_dir,
            dependencies,
        )
    except (OSError, subprocess.SubprocessError, UnicodeError, ValueError) as error:
        print(f"secureboot initramfs error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
