#!/usr/bin/env python3
"""Stage verified Signal/Java bytes for the ordinary signed runtime builder.

This performs no download, process launch, profile creation or login. The CLI
accepts the repository pin only; tests supply synthetic archives to the same
staging function. The output is a plain-file tree suitable for --include.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tarfile
import tempfile

PIN = Path(__file__).resolve().parents[1] / "external-assets/signal-runtime.json"
MAX_FILES = 8192
MAX_BYTES = 1024 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def verified_asset(directory, asset):
    name = asset["name"]
    require(Path(name).name == name, "invalid asset name")
    path = directory / name
    require(path.is_file(), "missing pinned runtime asset: " + name)
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    require(digest == asset["sha256"], "runtime asset digest differs: " + name)
    return path


def unpack(archive, root, destination, max_files=MAX_FILES, max_bytes=MAX_BYTES):
    with tarfile.open(archive) as contents:
        members = contents.getmembers()
        require(len(members) <= max_files, "runtime archive has too many entries")
        require(sum(member.size for member in members) <= max_bytes,
                "runtime archive exceeds size bound")
        for member in members:
            path = Path(member.name)
            require(not path.is_absolute() and ".." not in path.parts,
                    "runtime archive path escapes its root")
            require(path.parts == () or path.parts[0] == root,
                    "runtime archive has an unexpected root")
            require(member.isfile() or member.isdir() or member.issym(),
                    "runtime archive contains a special file")
        contents.extractall(destination, members=members, filter="data")
    extracted = destination / root
    require(extracted.is_dir() and not extracted.is_symlink(), "missing runtime root")
    for path in extracted.rglob("*"):
        require(path.resolve().is_relative_to(extracted.resolve()),
                "runtime archive link escapes its root")
        require(path.is_file() or (path.is_dir() and not path.is_symlink()),
                "runtime archive contains a directory link or missing target")
    return extracted


def require_x86_elf(path):
    with path.open("rb") as stream:
        header = stream.read(20)
    require(len(header) == 20 and header[:6] == b"\x7fELF\x02\x01"
            and int.from_bytes(header[18:20], "little") == 62,
            "Java runtime has a foreign executable")


def stage(assets, output, pin, architecture):
    require(architecture == pin["architecture"] == "x86_64",
            "pinned Signal native runtime supports only x86_64")
    require(not output.exists(), "runtime staging output already exists")
    verified = {name: verified_asset(assets, entry)
                for name, entry in pin["assets"].items()}
    with tempfile.TemporaryDirectory(prefix="tilo-signal-runtime-") as directory:
        work = Path(directory)
        signal = unpack(verified["signal"], pin["assets"]["signal"]["root"], work / "signal")
        java = unpack(verified["java"], pin["assets"]["java"]["root"], work / "java")
        require_x86_elf(java / "bin/java")
        require('JAVA_VERSION="' + pin["java_version"] + '"' in (java / "release").read_text(),
                "Java runtime version differs")
        require((java / "legal/java.base/LICENSE").is_file(), "Java runtime license is missing")
        require((signal / "lib" / ("signal-cli-" + pin["signal_version"] + ".jar")).is_file(),
                "Signal runtime version differs")
        require((signal / "lib/libsignal-client-0.102.1.jar").is_file(),
                "Signal native library is missing")
        # Resolve reviewed in-tree legal-file links. Runtime bundles sign plain
        # bytes and must not depend on an installation outside the bundle.
        output.mkdir(parents=True)
        try:
            shutil.copytree(java, output / "java", symlinks=False)
            shutil.copytree(signal / "lib", output / "signal/lib", symlinks=False)
            shutil.copyfile(verified["license"], output / "signal/LICENSE")
            shutil.copyfile(verified["source"], output / "signal/source.tar.gz")
            (output / "provenance.json").write_text(json.dumps(pin, indent=2) + "\n")
        except BaseException:
            shutil.rmtree(output)
            raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--arch", required=True)
    args = parser.parse_args()
    stage(args.assets, args.out, json.loads(PIN.read_text()), args.arch)


if __name__ == "__main__":
    main()
