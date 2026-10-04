#!/usr/bin/env python3
"""Boot an image's signed UKI and prove its splash fills a high-resolution panel.

    os/dev/check-boot-splash.py docker://REGISTRY/IMAGE@DIGEST [--out DIR] [--arch ARCH]
    os/dev/check-boot-splash.py --uki PATH [--out DIR]

A real boot, not a rendering of the theme: the image's own UKI starts from an
ESP beside an encrypted Tilo root, exactly the layout an installed machine has.
The firmware's framebuffer comes up at its default mode while the virtual
panel's native mode is larger, the same split a laptop shows between its GOP
mode and the GPU driver's native mode. The check films the boot and holds every
phase to the screen it is on:

  * systemd-stub's splash: the stub clears the screen to black and centres a
    fixed-size tile, so the tile's own edge must be black too, or the panel
    shows a box floating in its middle;
  * Plymouth, before and after the disk passphrase is accepted: the splash
    covers the whole native mode (no black bars at any edge) and its mark is
    centred on it, which fails if the theme kept a layout computed for the
    firmware framebuffer it started on.

The passphrase unlocks an empty volume, so the boot ends in the initramfs after
the unlock; everything up to there is the shipped boot chain. Frames are kept in
--out for review.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
from pathlib import Path

PASSPHRASE = "tilo-splash"
NATIVE = (2944, 1840)
ESP_TYPE = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
TILO_ROOT_TYPE = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab"
STUB_TILE = (1024, 768)
# The darkest Plymouth background row is (9, 10, 15); a bar the splash did not
# paint is (0, 0, 0).
PAINTED = 12
BLACK = 3

FIRMWARE = {
    "x86_64": (
        ["/usr/share/edk2/ovmf/OVMF_CODE.fd", "/usr/share/OVMF/OVMF_CODE_4M.fd",
         "/usr/share/OVMF/OVMF_CODE.fd"],
        ["/usr/share/edk2/ovmf/OVMF_VARS.fd", "/usr/share/OVMF/OVMF_VARS_4M.fd",
         "/usr/share/OVMF/OVMF_VARS.fd"],
    ),
    "aarch64": (
        ["/usr/share/edk2/aarch64/QEMU_EFI-silent-pflash.raw",
         "/usr/share/AAVMF/AAVMF_CODE.fd", "/usr/share/AAVMF/AAVMF_CODE.no-secboot.fd"],
        ["/usr/share/edk2/aarch64/vars-template-pflash.raw",
         "/usr/share/AAVMF/AAVMF_VARS.fd"],
    ),
}


def log(message: str) -> None:
    print(f"[boot-splash] {message}", file=sys.stderr, flush=True)


def fail(message: str) -> "NoReturn":  # type: ignore[valid-type]
    print(f"[boot-splash] FAIL: {message}", file=sys.stderr, flush=True)
    raise SystemExit(1)


def first_existing(paths: list[str]) -> str:
    for path in paths:
        if os.path.exists(path):
            return path
    fail(f"none of {paths} exists")


def run(*args: str, **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(args, check=True, **kwargs)


def extract_uki(image: str, work: Path) -> Path:
    """The UKI an image would install, from a skopeo transport reference.

    Only the image's layers are read, newest first, so the runner needs
    neither a container engine nor room for the unpacked root filesystem.
    """
    layout = work / "layout"
    run("skopeo", "copy", "--quiet", image, f"oci:{layout}:image")
    blobs = layout / "blobs"

    def blob(digest: str) -> Path:
        algorithm, value = digest.split(":", 1)
        return blobs / algorithm / value

    index = json.loads((layout / "index.json").read_text())
    manifest = json.loads(blob(index["manifests"][0]["digest"]).read_text())
    for layer in reversed(manifest["layers"]):
        path = blob(layer["digest"])
        with open(path, "rb") as handle:
            magic = handle.read(4)
        if magic == b"\x28\xb5\x2f\xfd":
            source = subprocess.Popen(["zstd", "-dcq", str(path)], stdout=subprocess.PIPE)
            stream, mode = source.stdout, "r|"
        else:
            source, stream, mode = None, open(path, "rb"), "r|*"
        found = None
        try:
            with tarfile.open(fileobj=stream, mode=mode) as archive:
                for member in archive:
                    name = member.name.removeprefix("./")
                    if member.isfile() and re.fullmatch(r"boot/EFI/Linux/[^/]+\.efi", name):
                        found = work / Path(name).name
                        with archive.extractfile(member) as data, open(found, "wb") as out:
                            shutil.copyfileobj(data, out)
                        break
        finally:
            stream.close()
            if source is not None:
                source.kill()
                source.wait()
        if found is not None:
            shutil.rmtree(layout)
            return found
    fail(f"{image} has no UKI in /boot/EFI/Linux")


def build_disk(uki: Path, arch: str, work: Path) -> Path:
    """An ESP holding the UKI as the removable-media loader, then a LUKS2 Tilo root."""
    esp, root, disk = work / "esp.img", work / "root.img", work / "disk.raw"
    esp_mib, root_mib = uki.stat().st_size // (1 << 20) + 64, 64
    loader = "BOOTX64.EFI" if arch == "x86_64" else "BOOTAA64.EFI"
    with open(esp, "wb") as handle:
        handle.truncate(esp_mib << 20)
    run("mkfs.vfat", "-F", "32", str(esp), capture_output=True)
    run("mmd", "-i", str(esp), "::/EFI", "::/EFI/BOOT")
    run("mcopy", "-i", str(esp), str(uki), f"::/EFI/BOOT/{loader}")
    with open(root, "wb") as handle:
        handle.truncate(root_mib << 20)
    # A cheap KDF: the unlock is timed under TCG, and the volume holds nothing.
    run("cryptsetup", "luksFormat", "--batch-mode", "--disable-locks",
        "--type", "luks2", "--pbkdf", "pbkdf2", "--pbkdf-force-iterations", "1000",
        "--key-file", "-", str(root), input=PASSPHRASE.encode())
    with open(disk, "wb") as handle:
        handle.truncate((1 + esp_mib + root_mib + 1) << 20)
    layout = (f"label: gpt\nstart=2048, size={esp_mib}M, type={ESP_TYPE}\n"
              f"size={root_mib}M, type={TILO_ROOT_TYPE}\n")
    run("sfdisk", "-q", str(disk), input=layout.encode())
    run("dd", f"if={esp}", f"of={disk}", "bs=1M", "seek=1", "conv=notrunc", "status=none")
    run("dd", f"if={root}", f"of={disk}", "bs=1M", f"seek={1 + esp_mib}",
        "conv=notrunc", "status=none")
    esp.unlink()
    root.unlink()
    return disk


class Monitor:
    def __init__(self, path: Path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(30)
        for _ in range(100):
            try:
                self.sock.connect(str(path))
                break
            except OSError:
                time.sleep(0.1)
        else:
            fail("QEMU monitor did not come up")
        self.read()

    def read(self) -> str:
        data = b""
        while not data.endswith(b"(qemu) "):
            chunk = self.sock.recv(65536)
            if not chunk:
                break
            data += chunk
        return data.decode(errors="replace")

    def command(self, line: str) -> str:
        self.sock.sendall(line.encode() + b"\n")
        return self.read()


class Frame:
    def __init__(self, path: Path):
        data = path.read_bytes()
        fields, offset = [], 0
        while len(fields) < 4:
            while data[offset:offset + 1].isspace():
                offset += 1
            if data[offset:offset + 1] == b"#":
                offset = data.index(b"\n", offset)
                continue
            end = offset
            while not data[end:end + 1].isspace():
                end += 1
            fields.append(data[offset:end])
            offset = end
        if fields[0] != b"P6" or fields[3] != b"255":
            raise ValueError(f"{path} is not an 8-bit PPM")
        self.width, self.height = int(fields[1]), int(fields[2])
        self.pixels = memoryview(data)[offset + 1:]

    def size(self) -> tuple[int, int]:
        return (self.width, self.height)

    def pixel(self, x: int, y: int) -> tuple[int, int, int]:
        at = (y * self.width + x) * 3
        return tuple(self.pixels[at:at + 3])  # type: ignore[return-value]

    def row(self, y: int, x0: int = 0, x1: int | None = None, step: int = 1):
        for x in range(x0, self.width if x1 is None else x1, step):
            yield self.pixel(x, y)

    def column(self, x: int, y0: int = 0, y1: int | None = None, step: int = 1):
        for y in range(y0, self.height if y1 is None else y1, step):
            yield self.pixel(x, y)

    def brightness(self) -> float:
        step = max(1, self.width * self.height // 20000) * 3
        sample = self.pixels[::step]
        return sum(sample) / max(1, len(sample))


def is_splash(frame: Frame) -> bool:
    """Plymouth paints its gradient across the whole frame; a console and the
    stub's black surround do not."""
    return min(frame.pixel(frame.width // 2, 1)) >= 9


def is_stub(frame: Frame) -> bool:
    """The stub's tile centred on black: the middle dot of its mark is lit."""
    width, height = frame.size()
    x = (width - STUB_TILE[0]) // 2 + STUB_TILE[0] // 2
    y = (height - STUB_TILE[1]) // 2 + round(STUB_TILE[1] * 0.43)
    return width >= STUB_TILE[0] and height >= STUB_TILE[1] \
        and max(frame.pixel(0, 0)) <= BLACK and max(frame.pixel(x, y)) > 150


def check_stub(frame: Frame) -> list[str]:
    """The stub's tile must not show its outline on the black screen around it."""
    width, height = frame.size()
    if (width, height) == STUB_TILE:
        return []
    x0, y0 = (width - STUB_TILE[0]) // 2, (height - STUB_TILE[1]) // 2
    ring = [*frame.row(y0, x0, x0 + STUB_TILE[0], 4),
            *frame.row(y0 + STUB_TILE[1] - 1, x0, x0 + STUB_TILE[0], 4),
            *frame.column(x0, y0, y0 + STUB_TILE[1], 4),
            *frame.column(x0 + STUB_TILE[0] - 1, y0, y0 + STUB_TILE[1], 4)]
    worst = max(max(pixel) for pixel in ring)
    if worst > BLACK:
        return [f"the UKI splash tile's edge is {worst}/255, not black: a "
                f"{STUB_TILE[0]}x{STUB_TILE[1]} box shows on the {width}x{height} screen"]
    return []


def check_splash(frame: Frame) -> list[str]:
    problems = []
    width, height = frame.size()
    edges = {
        "top": frame.row(0, step=8), "bottom": frame.row(height - 1, step=8),
        "left": frame.column(0, step=8), "right": frame.column(width - 1, step=8),
    }
    for name, pixels in edges.items():
        unpainted = sum(1 for pixel in pixels if sum(pixel) < PAINTED)
        if unpainted:
            problems.append(f"{unpainted} unpainted pixels along the {name} edge")
    # The mark: the brightest pixels in the upper half, which only the logo
    # reaches. Its centre must sit on the screen's centre line.
    xs = []
    for y in range(0, height // 2, 4):
        for x in range(0, width, 4):
            if max(frame.pixel(x, y)) > 150:
                xs.append(x)
    if not xs:
        problems.append("no logo in the upper half of the splash")
    else:
        centre = (min(xs) + max(xs)) / 2
        if abs(centre - width / 2) > width * 0.02:
            problems.append(f"logo centred at x={centre:.0f}, screen centre is {width / 2:.0f}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("image", nargs="?",
                        help="skopeo reference (docker://..., oci-archive:...) of the image whose UKI to boot")
    parser.add_argument("--uki", type=Path, help="boot this UKI instead of an image's")
    parser.add_argument("--arch", default=platform.machine())
    parser.add_argument("--out", type=Path, default=None, help="keep frames here")
    parser.add_argument("--timeout", type=int, default=1500, help="seconds")
    args = parser.parse_args()
    if (args.image is None) == (args.uki is None):
        parser.error("name an image or a --uki")
    if args.image and not (shutil.which("skopeo") and shutil.which("zstd")):
        fail("skopeo and zstd are required to read an image")
    for tool in ("mkfs.vfat", "mmd", "mcopy", "cryptsetup", "sfdisk",
                 f"qemu-system-{args.arch}"):
        if not shutil.which(tool):
            fail(f"{tool} is required")

    work = Path(tempfile.mkdtemp(prefix="tilo-boot-splash.", dir=os.environ.get("TMPDIR", "/var/tmp")))
    out = args.out or work / "frames"
    out.mkdir(parents=True, exist_ok=True)
    try:
        uki = args.uki.resolve() if args.uki else extract_uki(args.image, work)
        disk = build_disk(uki, args.arch, work)
        code, vars_template = (first_existing(paths) for paths in FIRMWARE[args.arch])
        vars_file = work / "vars.fd"
        shutil.copyfile(vars_template, vars_file)
        accel = "kvm" if os.access("/dev/kvm", os.R_OK | os.W_OK) and \
            args.arch == platform.machine() else "tcg"
        xres, yres = NATIVE
        qemu = [f"qemu-system-{args.arch}", "-accel", accel, "-m", "3072", "-smp", "4",
                "-drive", f"if=pflash,format=raw,readonly=on,file={code}",
                "-drive", f"if=pflash,format=raw,file={vars_file}",
                "-drive", f"file={disk},format=raw,if=virtio",
                # No network: the initramfs needs none, and a default NIC
                # makes QEMU require an iPXE option ROM the host may lack.
                "-nic", "none",
                "-display", "none", "-serial", f"file:{work / 'serial.log'}",
                "-monitor", f"unix:{work / 'mon.sock'},server,nowait"]
        if args.arch == "x86_64":
            qemu += ["-machine", "q35", "-device", f"virtio-vga,xres={xres},yres={yres}"]
        else:
            cpu = "host" if accel == "kvm" else "max"
            qemu += ["-machine", "virt", "-cpu", cpu,
                     "-device", f"virtio-gpu-pci,xres={xres},yres={yres}",
                     "-device", "qemu-xhci", "-device", "usb-kbd"]
        log(f"booting {uki} ({accel}, panel {xres}x{yres})")
        vm = subprocess.Popen(qemu)
        problems: list[str] = []
        try:
            try:
                monitor = Monitor(work / "mon.sock")
            except OSError as error:
                vm.wait(timeout=10)
                fail(f"QEMU exited ({vm.returncode}) before its monitor answered: {error}")
            deadline = time.monotonic() + args.timeout
            stub_frames = splash_frames = 0
            unlocked_at = None
            typed_at = None
            index = 0
            while time.monotonic() < deadline and vm.poll() is None:
                index += 1
                path = out / f"{index:04d}.ppm"
                monitor.command(f"screendump {path}")
                if not path.exists():
                    time.sleep(1)
                    continue
                frame = Frame(path)
                found: list[str] = []
                if is_splash(frame):
                    # Plymouth, on the firmware framebuffer or on the GPU
                    # driver's native mode: either way the whole screen.
                    found = check_splash(frame)
                    if frame.size() == NATIVE:
                        splash_frames += 1
                    # Type once the prompt has had time to appear, and again if
                    # the splash is still asking a while later.
                    if splash_frames >= 3 and unlocked_at is None and \
                            (typed_at is None or time.monotonic() - typed_at > 60):
                        for char in PASSPHRASE:
                            monitor.command(f"sendkey {'minus' if char == '-' else char}")
                        monitor.command("sendkey ret")
                        typed_at = time.monotonic()
                        log(f"typed the passphrase at frame {index}")
                elif is_stub(frame):
                    stub_frames += 1
                    found = check_stub(frame)
                elif typed_at is not None:
                    # Past the unlock: the empty volume ends the boot in the
                    # initramfs. Keep filming briefly for the record.
                    unlocked_at = unlocked_at or time.monotonic()
                    if time.monotonic() - unlocked_at > 30:
                        break
                elif index % 10 == 0:
                    log(f"frame {index}: {frame.width}x{frame.height}, waiting for the splash")
                problems += [f"{path.name}: {issue}" for issue in found]
                # A native frame is 16 MB: keep the failing ones and a sample.
                if not found and index % 10:
                    path.unlink()
                time.sleep(2 if accel == "tcg" else 0.5)
        finally:
            vm.kill()
            vm.wait()
        log(f"{stub_frames} UKI splash frames, {splash_frames} Plymouth frames at "
            f"{xres}x{yres}; frames in {out}")
        if splash_frames < 3:
            tail = (work / "serial.log").read_text(errors="replace")[-3000:]
            fail(f"Plymouth never covered the {xres}x{yres} panel\n{tail}")
        if unlocked_at is None:
            fail("the passphrase prompt did not accept the passphrase")
        if problems:
            for problem in problems[:40]:
                print(f"  {problem}", file=sys.stderr)
            fail(f"{len(problems)} splash frames do not fill the panel")
        log("the boot splash fills the panel in every phase")
        return 0
    finally:
        for target in [work / name for name in ("disk.raw", "vars.fd", "layout")] \
                + list(work.glob("*.efi")):
            if target.is_dir():
                shutil.rmtree(target, ignore_errors=True)
            elif target.exists():
                target.unlink()


if __name__ == "__main__":
    sys.exit(main())
