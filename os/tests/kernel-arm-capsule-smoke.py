#!/usr/bin/env python3
"""Boot disposable ARM console acceptance with signed kernel modules and read-only inputs.

Run --mode probe first. Native OCI root is mounted, never exported/copied. All
guest writes live in RAM; no host disk device or writable directory is shared.
Console mode executes the generated production Landlock capsule plans. Dummy
display/render sources mean this does not establish graphical acceptance.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile


def run(arguments, **kwargs):
    return subprocess.run(arguments, check=True, text=True, **kwargs)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("kernel", "modules", "x86-rootfs", "proton", "steam-runtime",
                 "umu", "test-binary", "launcher", "helper", "manifests", "output"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--native-image", required=True)
    parser.add_argument("--guest-squashfs", type=Path,
                        help="read-only guest-root SquashFS file over virtio-block instead of 9p")
    parser.add_argument("--qemu", type=Path, help="explicit QEMU binary or recorded loader wrapper")
    parser.add_argument("--cpu", default="max",
                        help="QEMU ARM CPU model and feature options; recorded with the result (default: max)")
    parser.add_argument("--mode", choices=("probe", "console", "namespace-diagnostic", "fex-diagnostic", "wine-diagnostic"), default="probe")
    parser.add_argument("--inside-unshare", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if os.uname().machine != "aarch64":
        parser.error("requires native ARM Linux build tools and QEMU")
    if not args.inside_unshare:
        return subprocess.call(["podman", "unshare", sys.executable, str(Path(__file__).resolve()),
                                *sys.argv[1:], "--inside-unshare"])
    image = json.loads(run(["podman", "image", "inspect", args.native_image],
                           stdout=subprocess.PIPE).stdout)[0]["Id"]
    mounts = json.loads(run(["podman", "image", "mount", "--format", "json"],
                            stdout=subprocess.PIPE).stdout)
    if any(image.removeprefix("sha256:") in json.dumps(entry) for entry in mounts):
        parser.error("native image is already mounted; refusing to take ownership of that view")
    args.output.mkdir(parents=False, exist_ok=False)
    out = args.output.resolve()
    root = out / "initramfs"
    inputs = out / "inputs"
    inputs.mkdir()
    for directory in ("modules", "usr/lib", "usr/lib64", "lib", "lib64"):
        (root / directory).mkdir(parents=True, exist_ok=True)
    fixtures = Path(__file__).resolve().parent / "fixtures"
    run(["cc", "-O2", "-Wall", "-Wextra", "-Werror",
         str(fixtures / "arm-capsule-vm-init.c"), "-o", str(root / "init")])
    dependencies = run(["ldd", str(root / "init")], stdout=subprocess.PIPE).stdout
    if "not found" in dependencies:
        raise RuntimeError(dependencies)
    for name in re.findall(r"(?:=>\s+|^\s*)(/\S+)", dependencies, re.M):
        destination = root / name.lstrip("/")
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(name, destination)
    # Read only the matching signed modules; do not extract arbitrary archive paths.
    modules = {"netfs", "9pnet", "9pnet_virtio", "9p", "overlay"}
    if args.guest_squashfs:
        args.guest_squashfs = args.guest_squashfs.resolve(strict=True)
        if not args.guest_squashfs.is_file():
            raise ValueError("guest block input must be a regular artifact file, never a device")
        with args.guest_squashfs.open("rb") as stream:
            if stream.read(4) != b"hsqs":
                raise ValueError("guest block input must be a SquashFS image file")
        modules.add("squashfs")
        (root / "guest-squashfs").touch()
    with tarfile.open(args.modules) as archive:
        for entry in archive:
            name = Path(entry.name).name
            if name.removesuffix(".ko") in modules and name.endswith(".ko"):
                data = archive.extractfile(entry).read()
                if not data.endswith(b"~Module signature appended~\n"):
                    raise RuntimeError(f"unsigned module: {entry.name}")
                (root / "modules" / name).write_bytes(data)
                modules.remove(name.removesuffix(".ko"))
    if modules:
        raise RuntimeError(f"missing modules: {modules}")
    artifacts = {"compat-tests": args.test_binary, "tilo-launcher": args.launcher,
                 "tilo-windows-runtime": args.helper, "umu_run.py": args.umu,
                 "run.sh": fixtures / "arm-capsule-vm-run.sh"}
    for name in ("fex-emulator.json", "fex-graphics-provider.json"):
        artifacts[name] = args.manifests / name
    records = {"native_image_id": image, "mode": args.mode, "cpu": args.cpu,
               "kernel_sha256": digest(args.kernel),
               "module_archive_sha256": digest(args.modules),
               "signed_modules": {path.name: digest(path) for path in sorted((root / "modules").iterdir())},
               "inputs": {}}
    if args.guest_squashfs:
        records["guest_squashfs"] = {"path": str(args.guest_squashfs),
                                     "sha256": digest(args.guest_squashfs)}
    qemu_binary = Path(args.qemu or shutil.which("qemu-system-aarch64") or "/missing-qemu").resolve(strict=True)
    records["qemu"] = {"path": str(qemu_binary), "sha256": digest(qemu_binary),
                       "version": run([str(qemu_binary), "--version"], stdout=subprocess.PIPE).stdout.strip()}
    for name, source in artifacts.items():
        shutil.copyfile(source, inputs / name)
        (inputs / name).chmod(0o755 if name in ("compat-tests", "tilo-launcher", "tilo-windows-runtime") else 0o644)
        records["inputs"][name] = digest(source)
    (inputs / "mode").write_text(args.mode + "\n")
    (out / "inputs.json").write_text(json.dumps(records, indent=2) + "\n")
    # Pipe directory names as data, not through shell interpolation.
    with (out / "initramfs.cpio.gz").open("wb") as archive:
        find = subprocess.Popen(["find", ".", "-print0"], cwd=root, stdout=subprocess.PIPE)
        cpio = subprocess.Popen(["cpio", "--null", "-o", "--format=newc", "--quiet"], cwd=root,
                                stdin=find.stdout, stdout=subprocess.PIPE)
        find.stdout.close()
        gzip = subprocess.Popen(["gzip", "-1"], stdin=cpio.stdout, stdout=archive)
        cpio.stdout.close()
        statuses = [gzip.wait(), cpio.wait(), find.wait()]
        if any(statuses):
            raise RuntimeError(f"initramfs creation failed: {statuses}")
    native = run(["podman", "image", "mount", image], stdout=subprocess.PIPE).stdout.strip()
    try:
        qemu = [str(qemu_binary), "-machine", "virt,gic-version=3", "-cpu", args.cpu,
                "-accel", "tcg", "-smp", "2", "-m", "4096", "-nographic", "-no-reboot",
                "-nic", "none", "-kernel", str(args.kernel.resolve()),
                "-initrd", str(out / "initramfs.cpio.gz"),
                "-append", "console=ttyAMA0 rdinit=/init panic=1"]
        shares = {"native": Path(native), "inputs": inputs, "x86": args.x86_rootfs,
                  "proton": args.proton, "steam": args.steam_runtime}
        if args.guest_squashfs:
            del shares["x86"]
            if "," in str(args.guest_squashfs):
                raise ValueError("QEMU image paths must not contain commas")
            qemu += ["-drive", f"if=none,id=guestroot,file={args.guest_squashfs},format=raw,readonly=on",
                     "-device", "virtio-blk-pci,drive=guestroot"]
        for tag, path in shares.items():
            path = path.resolve(strict=True)
            if "," in str(path):
                raise ValueError("QEMU share paths must not contain commas")
            qemu += ["-fsdev", f"local,id={tag},path={path},security_model=none,readonly=on",
                     "-device", f"virtio-9p-pci,fsdev={tag},mount_tag={tag}"]
        (out / "qemu-command.json").write_text(json.dumps(qemu, indent=2) + "\n")
        with (out / "serial.log").open("w") as serial:
            result = subprocess.run(["timeout", "--kill-after=10s",
                                     "180" if args.mode == "probe" else "2100", *qemu],
                                    stdout=serial, stderr=subprocess.STDOUT)
        log = (out / "serial.log").read_text(errors="replace")
        accepted = result.returncode == 0 and "\nTILO_ARM_CAPSULE_VM_PASS\n" in log
        expected = ["TILO_ARM_CAPSULE_TRANSPORT_PASS"]
        if args.mode == "console":
            expected += [f"TILO_ARM_CAPSULE_CASE_PASS={kind}" for kind in ("wine64", "wine32", "proton")]
        elif args.mode == "namespace-diagnostic":
            expected += ["TILO_ARM_CAPSULE_CASE_PASS=wine64"]
        elif args.mode == "fex-diagnostic":
            expected += ["TILO_FEX_DIAGNOSTIC_PASS"]
        elif args.mode == "wine-diagnostic":
            expected += ["TILO_DIRECT_WINE_DIAGNOSTIC_PASS"]
        accepted &= all(marker in log for marker in expected)
        scope = {"probe": "transport-and-shim", "console": "console-capsules-with-Landlock",
                 "namespace-diagnostic": "namespace-only-diagnostic-NOT-Landlock-acceptance",
                 "fex-diagnostic": "direct-FEX-diagnostic-NOT-capsule-acceptance",
                 "wine-diagnostic": "direct-Wine-diagnostic-NOT-capsule-acceptance"}[args.mode]
        passed = accepted
        diagnostic = args.mode.endswith("-diagnostic")
        summary = {"accepted": passed and not diagnostic, "diagnostic_passed": passed if diagnostic else None,
                   "mode": args.mode, "cpu": args.cpu, "qemu_exit": result.returncode,
                   "scope": scope}
        (out / "result.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps(summary), flush=True)
        return 0 if passed else 1
    finally:
        run(["podman", "image", "unmount", image])


if __name__ == "__main__":
    raise SystemExit(main())
