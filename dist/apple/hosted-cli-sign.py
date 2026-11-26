#!/usr/bin/env python3
"""Sign or verify the two programs in one tested, thin hosted CLI archive."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import struct
import sys
import tarfile
import tempfile

if sys.version_info < (3, 11):
    raise SystemExit("hosted CLI signing requires Python 3.11 or newer")

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "macos"))
from collection_signing_macho import compare_signed_macho, inspect_macho

PROGRAMS = ("tilo", "tilo-runtime")
IDENTIFIER = "so.tilo.cli.hosted"
ARCHITECTURES = {"aarch64": "arm64", "x86_64": "x86_64"}


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def program_member(name, size):
    member = tarfile.TarInfo(name)
    member.mode, member.size = 0o755, size
    member.uname = member.gname = "root"
    return member


def unpack(archive, destination, expected=None):
    # Hash and unpack the same private snapshot even if a download is replaced.
    destination.mkdir()
    snapshot = destination.parent / (destination.name + ".tar.gz")
    if not archive.is_file() or archive.is_symlink():
        raise ValueError("the input archive must be a regular file")
    shutil.copyfile(archive, snapshot)
    actual = digest(snapshot)
    if expected is not None and actual != expected:
        raise ValueError("archive SHA-256 differs from its trusted receipt")
    # Both release packers use plain USTAR. Examine a single fixed header before
    # any size-dependent read. In particular, never interpret PAX or GNU extensions.
    with gzip.open(snapshot, "rb") as package:
        for name in PROGRAMS:
            header = package.read(tarfile.BLOCKSIZE)
            if len(header) != tarfile.BLOCKSIZE:
                raise ValueError("truncated CLI archive header")
            member = tarfile.TarInfo.frombuf(header, "ascii", "strict")
            if (not 0 < member.size <= 2**31
                    or header != program_member(name, member.size).tobuf(format=tarfile.USTAR_FORMAT)):
                raise ValueError(f"expected the regular executable {name} in the CLI archive")
            with (destination / name).open("xb") as output:
                remaining = member.size
                while remaining:
                    chunk = package.read(min(remaining, 1024 * 1024))
                    if not chunk:
                        raise ValueError(f"truncated CLI archive member {name}")
                    output.write(chunk)
                    remaining -= len(chunk)
            padding = -member.size % tarfile.BLOCKSIZE
            if package.read(padding) != bytes(padding):
                raise ValueError(f"invalid archive padding after {name}")
            (destination / name).chmod(0o755)
        # Reject extra entries, including archives hidden behind a tar end marker.
        tail = package.read(tarfile.RECORDSIZE + 1025)
        if len(tail) < 1024 or len(tail) > tarfile.RECORDSIZE + 1024 or any(tail):
            raise ValueError("unexpected data after the two CLI programs")
    return actual


def pack(programs, archive):
    with archive.open("xb") as file, gzip.GzipFile(filename="", fileobj=file, mode="wb", mtime=0) as zipped:
        with tarfile.open(fileobj=zipped, mode="w|", format=tarfile.USTAR_FORMAT) as package:
            for name in PROGRAMS:
                path = programs / name
                with path.open("rb") as source:
                    package.addfile(program_member(name, path.stat().st_size), source)


def product_requirement(team):
    # The login Keychain treats these two executables as the same Tilo product.
    # The product and team stay stable across a Developer ID certificate renewal.
    return (f'identifier "{IDENTIFIER}" and anchor apple generic '
            'and certificate 1[field.1.2.840.113635.100.6.2.6] exists '
            'and certificate leaf[field.1.2.840.113635.100.6.1.13] exists '
            f'and certificate leaf[subject.OU] = "{team}"')


def check_apple(programs, arch, identity, requirement):
    import collection_native as native
    from collection_staged_sign import verify_entitlements

    expected = native.run(["/usr/bin/csreq", "-r=designated => " + requirement, "-t"], combine=False)
    for name in PROGRAMS:
        path = programs / name
        native.verify_code_signature(path, identity, "distribution", arch, requirement)
        actual = native.run(["/usr/bin/codesign", "-d", "-r-", path], combine=False)
        if actual.strip() != expected.strip():
            raise ValueError(f"unexpected embedded designated requirement in {name}")
    verify_entitlements(programs, {name: {} for name in PROGRAMS})


def leaf_fingerprint(work, programs, sha1, expected):
    import collection_native as native

    fingerprints = set()
    for name in PROGRAMS:
        prefix = work / (name + "-cert-")
        native.run(["/usr/bin/codesign", "-d", "--extract-certificates", prefix, programs / name])
        leaf = Path(str(prefix) + "0").read_bytes()
        sha256 = hashlib.sha256(leaf).hexdigest()
        if hashlib.sha1(leaf).hexdigest().upper() != sha1 or (expected is not None and sha256 != expected.lower()):
            raise ValueError(f"unexpected Developer ID leaf certificate on {name}")
        fingerprints.add(sha256)
    if len(fingerprints) != 1:
        raise ValueError("the two programs have different Developer ID certificates")
    return fingerprints.pop()


def run(args, work):
    if args.mode == "sign" and os.path.lexists(args.signed):
        raise ValueError("the signed archive output already exists")
    raw, signed = work / "raw", work / "signed"
    arch = ARCHITECTURES[args.arch]
    raw_digest = unpack(args.raw, raw, args.sha256)
    for name in PROGRAMS:
        info = inspect_macho((raw / name).read_bytes(), architecture=arch)
        if struct.unpack_from("<I", info["header"], 12)[0] != 2:
            raise ValueError(f"{name} is not a Mach-O executable")
    if args.mode != "payload":
        if sys.platform != "darwin":
            raise ValueError("Apple Developer ID verification requires macOS")
        if not re.fullmatch(r"[A-Z0-9]{10}", args.team) or not re.fullmatch(r"[0-9a-fA-F]{40}", args.identity):
            raise ValueError("supply the trusted team ID and exact public Developer ID certificate fingerprint")
        if args.leaf_sha256 is not None and not re.fullmatch(r"[0-9a-fA-F]{64}", args.leaf_sha256):
            raise ValueError("supply the full SHA-256 of the trusted Developer ID leaf certificate")
        identity = {"team": args.team, "sha1": args.identity.upper(), "name": None}
        requirement = product_requirement(args.team)
    if args.mode == "sign":
        import collection_native as native

        available = native.run(["/usr/bin/security", "find-identity", "-v", "-p", "codesigning"])
        identity = native.select_identity(available, identity["sha1"], "distribution")
        if identity["team"] != args.team:
            raise ValueError("the signing certificate belongs to another Apple team")
        shutil.copytree(raw, signed)
        empty = work / "empty.entitlements"
        empty.write_bytes(plistlib.dumps({}))
        for name in PROGRAMS:
            native.run(["/usr/bin/codesign", "--force", "--sign", identity["sha1"], "--timestamp", "--digest-algorithm=sha256",
                        "--options", "runtime", "--identifier", IDENTIFIER, "--requirements",
                        "=designated => " + requirement, "--entitlements", empty, signed / name])
    else:
        signed_digest = unpack(args.signed, signed, args.signed_sha256)

    comparisons = {name: compare_signed_macho((raw / name).read_bytes(), (signed / name).read_bytes(),
                                              architecture=arch) for name in PROGRAMS}
    receipt = {"architecture": args.arch, "raw_sha256": raw_digest, "programs": comparisons}
    if args.mode != "payload":
        check_apple(signed, arch, identity, requirement)
        leaf = leaf_fingerprint(work, signed, identity["sha1"], args.leaf_sha256)
        receipt.update(team=identity["team"], leaf_sha1=identity["sha1"], leaf_sha256=leaf,
                       designated_requirement=requirement)
    if args.mode == "sign":
        output = work / "qualified.tar.gz"
        pack(signed, output)
        signed_digest = digest(output)
        # A failed check leaves no publishable archive. An earlier result is never replaced.
        os.link(output, args.signed)
    receipt["signed_sha256"] = signed_digest
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest="mode", required=True)
    for mode in ("sign", "verify", "payload"):
        cmd = modes.add_parser(mode, help={"sign": "sign on the credentialed operator Mac",
                                          "verify": "verify the public signatures on a Mac",
                                          "payload": "compare program contents on any platform"}[mode])
        cmd.add_argument("raw", type=Path)
        cmd.add_argument("sha256", help="SHA-256 from the trusted raw build")
        cmd.add_argument("arch", choices=ARCHITECTURES)
        cmd.add_argument("signed", type=Path)
        if mode != "sign":
            cmd.add_argument("--signed-sha256", help="verify the signed archive digest before unpacking")
        if mode != "payload":
            cmd.add_argument("--identity", required=True, help="pinned Developer ID Application leaf SHA-1")
            cmd.add_argument("--team", required=True, help="pinned Apple team ID")
            cmd.add_argument("--leaf-sha256", help="trusted SHA-256 of that same public DER certificate")
    args = parser.parse_args()
    try:
        parent = args.signed.parent if args.mode == "sign" else None
        with tempfile.TemporaryDirectory(dir=parent) as scratch:
            print(json.dumps(run(args, Path(scratch)), sort_keys=True))
    except (OSError, ValueError, tarfile.TarError, EOFError) as error:
        parser.exit(1, f"hosted-cli-sign: {error}\n")


if __name__ == "__main__":
    main()
