#!/usr/bin/env python3
"""Prepare an offline ATL qualification payload; never install or launch it."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import struct
import subprocess
import tempfile
import zipfile

ARCHES = {"x86_64": ("x86_64", 62), "aarch64": ("arm64-v8a", 183)}
MAX_APK_BYTES = 256 * 1024 * 1024
MAX_EXPANDED_BYTES = 1024 * 1024 * 1024


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def pinned(path, expected):
    if not re.fullmatch(r"[0-9a-f]{64}", expected) or digest(path) != expected:
        raise ValueError(f"SHA-256 mismatch: {path.name}")


def safe_relative(name):
    path = PurePosixPath(name)
    if (not name or "\\" in name or path.is_absolute()
            or any(part in ("", ".", "..") for part in name.split("/"))):
        raise ValueError("unsafe payload path")
    return path


def elf_machine(header):
    if len(header) < 20 or header[:6] != b"\x7fELF\x02\x01":
        raise ValueError("expected a 64-bit little-endian ELF")
    return struct.unpack_from("<H", header, 18)[0]


def inspect_apk(apk, arch, expected):
    pinned(apk, expected)
    if apk.stat().st_size > MAX_APK_BYTES:
        raise ValueError("APK exceeds qualification size limit")
    abi, machine = ARCHES[arch]
    with zipfile.ZipFile(apk) as archive:
        entries = archive.infolist()
        if len(entries) > 10000 or sum(e.file_size for e in entries) > MAX_EXPANDED_BYTES:
            raise ValueError("APK exceeds archive limits")
        names = set()
        native_abis = set()
        for entry in entries:
            safe_relative(entry.filename.rstrip("/") if entry.is_dir() else entry.filename)
            if entry.filename in names:
                raise ValueError("duplicate APK member")
            names.add(entry.filename)
            mode = entry.external_attr >> 16
            if stat.S_ISLNK(mode) or entry.flag_bits & 1:
                raise ValueError("symlink or encrypted APK member")
            parts = PurePosixPath(entry.filename).parts
            if len(parts) == 3 and parts[0] == "lib" and parts[2].endswith(".so"):
                native_abis.add(parts[1])
                if parts[1] == abi:
                    with archive.open(entry) as stream:
                        if elf_machine(stream.read(20)) != machine:
                            raise ValueError("APK native library disagrees with ABI directory")
        if "AndroidManifest.xml" not in names or "classes.dex" not in names:
            raise ValueError("qualification requires a standalone APK with manifest and DEX")
        if native_abis and abi not in native_abis:
            raise ValueError(f"APK lacks native libraries for {abi}; no CPU translation")
    return {"apk_sha256": expected, "architecture": arch,
            "native_abis": sorted(native_abis), "runtime_accepted": False}


def verify_signer(apk, verifier, expected):
    if not re.fullmatch(r"[0-9a-f]{64}", expected):
        raise ValueError("signer SHA-256 must be 64 lowercase hex digits")
    result = subprocess.run(
        [str(verifier), "verify", "--verbose", "--print-certs", str(apk)],
        capture_output=True, text=True, timeout=60, check=False,
    )
    # Do not echo verifier output: an APK's untrusted certificate DN is not a log field.
    signers = re.findall(r"^Signer #\d+ certificate SHA-256 digest: ([0-9a-fA-F]{64})$",
                         result.stdout, re.MULTILINE)
    if result.returncode or [s.lower() for s in signers] != [expected]:
        raise ValueError("APK signature invalid or signer does not match the pinned publisher")


def runtime_files(root):
    result = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        safe_relative(relative)
        if path.is_symlink() or not (path.is_dir() or path.is_file()):
            raise ValueError("runtime must contain only regular files and directories")
        if path.is_file():
            if path.stat().st_mode & (stat.S_ISUID | stat.S_ISGID):
                raise ValueError("runtime cannot contain setuid or setgid files")
            result[relative] = digest(path)
    return result


def verify_runtime(root, receipt_path, receipt_sha256, arch):
    pinned(receipt_path, receipt_sha256)
    receipt = json.loads(receipt_path.read_text())
    if not isinstance(receipt, dict) or receipt.get("schema") != 1 or receipt.get("architecture") != arch:
        raise ValueError("runtime receipt schema or architecture mismatch")
    files = runtime_files(root)
    if not files or files != receipt.get("files"):
        raise ValueError("runtime files differ from the pinned receipt")
    if not any(name.startswith("licenses/") for name in files):
        raise ValueError("runtime requires bundled license notices")
    binary = root / "bin/android-translation-layer"
    with binary.open("rb") as stream:
        if elf_machine(stream.read(20)) != ARCHES[arch][1]:
            raise ValueError("runtime ELF architecture mismatch")
    if not os.access(binary, os.X_OK):
        raise ValueError("runtime entrypoint is not executable")
    return receipt


def prepare(args):
    if not re.fullmatch(r"[A-Za-z_$][A-Za-z0-9_$/]*", args.activity):
        raise ValueError("invalid ATL activity")
    report = inspect_apk(args.apk, args.architecture, args.apk_sha256)
    verify_signer(args.apk, args.apksigner, args.signer_sha256)
    verify_runtime(args.runtime_root, args.runtime_receipt, args.runtime_receipt_sha256,
                   args.architecture)
    # A stable, content-addressed qualification identity; not an inferred Android package id.
    package_id = "so.tilo.android.qualify.h" + args.apk_sha256
    if args.output.resolve().is_relative_to(args.runtime_root.resolve()):
        raise ValueError("output must be outside the runtime input")
    if args.output.exists():
        raise ValueError("output already exists; qualification never overwrites a payload")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".apk-qualify-", dir=args.output.parent) as temp:
        stage = Path(temp) / "payload"
        stage.mkdir()
        shutil.copytree(args.runtime_root, stage / "runtime", symlinks=True)
        shutil.copyfile(args.apk, stage / "app.apk")
        # Recheck the copied inputs, so a changed source never publishes a mixed payload.
        pinned(stage / "app.apk", args.apk_sha256)
        verify_runtime(stage / "runtime", args.runtime_receipt,
                       args.runtime_receipt_sha256, args.architecture)
        manifest = ('[package]\n' + f'id = "{package_id}"\nversion = "0.0.0"\n'
                    '[entrypoint]\nbin = "runtime/bin/android-translation-layer"\n'
                    '[sandbox]\nseccomp = "tilo-default"\n[sandbox.net]\nmode = "off"\n')
        if getattr(args, 'notifications', False):
            manifest += '[sandbox.portals]\nnotifications = "via-host"\n'
        (stage / "tilo.toml").write_text(manifest)
        (stage / "activity.txt").write_text(args.activity + "\n")
        # These are argv tokens, never a shell command. The normal launcher supplies
        # per-workspace XDG data, Wayland and process-tree confinement.
        command = ["tilo-launcher", "run", "--manifest", str(args.output / "tilo.toml"),
                   "--payload-root", str(args.output), "--no-services",
                   "--workspace-id", args.workspace_id,
                   "--wayland-socket", str(args.wayland_socket)] if args.wayland_socket else None
        report.update(signature_verified=True, signer_sha256=args.signer_sha256,
                      notification_provider_declared=getattr(args, 'notifications', False),
                      runtime_receipt_sha256=args.runtime_receipt_sha256,
                      runtime_policy={"activity": args.activity,
                                      "art_options": ["-Xnoimage-dex2oat", "-Xusejit:false"]},
                      launcher_argv=command, desktop_action={"provider": "applications", "id": package_id},
                      prepared=True)
        (stage / "qualification.json").write_text(json.dumps(report, indent=2) + "\n")
        stage.rename(args.output)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("apk", type=Path)
    parser.add_argument("--apk-sha256", required=True)
    parser.add_argument("--architecture", choices=ARCHES, required=True)
    parser.add_argument("--output", type=Path,
                        help="prepare a payload; omit for archive-only inspection")
    parser.add_argument("--runtime-root", type=Path)
    parser.add_argument("--runtime-receipt", type=Path)
    parser.add_argument("--runtime-receipt-sha256")
    parser.add_argument("--apksigner", type=Path)
    parser.add_argument("--signer-sha256")
    parser.add_argument("--activity", help="explicit slash-separated ATL activity")
    parser.add_argument("--notifications", action="store_true",
                        help="explicitly declare filtered host notifications in this developer fixture")
    parser.add_argument("--workspace-id")
    parser.add_argument("--wayland-socket", type=Path)
    args = parser.parse_args()
    try:
        args.apk = args.apk.resolve(strict=True)
        if args.output:
            required = ("runtime_root", "runtime_receipt", "runtime_receipt_sha256",
                        "apksigner", "signer_sha256", "activity")
            if any(not getattr(args, key) for key in required):
                raise ValueError("preparation requires runtime pins, signer and activity")
            if not re.fullmatch(r"[A-Za-z_$][A-Za-z0-9_$/]*", args.activity):
                raise ValueError("invalid ATL activity")
            if bool(args.workspace_id) != bool(args.wayland_socket):
                raise ValueError("standalone preflight requires both workspace and socket")
            if args.workspace_id and not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", args.workspace_id):
                raise ValueError("invalid workspace id")
            for key in ("runtime_root", "runtime_receipt", "apksigner"):
                setattr(args, key, getattr(args, key).resolve(strict=True))
            args.output = args.output.absolute()
            if args.wayland_socket:
                args.wayland_socket = args.wayland_socket.absolute()
                if not stat.S_ISSOCK(args.wayland_socket.stat().st_mode):
                    raise ValueError("preflight requires an existing isolated Wayland socket")
            report = prepare(args)
        else:
            report = inspect_apk(args.apk, args.architecture, args.apk_sha256)
            report.update(prepared=False, signature_verified=False)
        print(json.dumps(report, indent=2))
    except (ValueError, OSError, zipfile.BadZipFile, subprocess.TimeoutExpired) as error:
        parser.exit(2, f"qualification refused: {error}\n")


if __name__ == "__main__":
    main()
