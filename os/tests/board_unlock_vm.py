#!/usr/bin/env python3
"""A board image's UKI takes a typed passphrase at its unlock prompt.

    board_unlock_vm.py UKI WORKDIR

The UKI boots as the firmware boots it on the board: from an ESP as the removable-media
loader, through its own systemd-stub, beside a LUKS2 Tilo root, with its own command line and
initramfs. Only its device tree is swapped, for QEMU virt's own plus a ramoops region in the
board's layout, so everything the board's command line and initramfs start in the initrd
(the console log level unit, journald's forwarding to the kernel log, the watchdog arguments)
runs as on the board. The keyboard is a USB keyboard on QEMU's xHCI.

A wrong passphrase is typed first: its rejection proves the keystrokes reached the agent, and
then the right one must open the volume. What this leaves out is the board's own input path
below evdev (its I2C controller and I2C HID), which a VM cannot reach. Exits nonzero when
the prompt does not take the typed passphrase.
"""
import os, re, shutil, socket, struct, subprocess, sys, time
from pathlib import Path

PASSPHRASE = "tilo-unlock"
WRONG = "not-the-passphrase"
ESP_TYPE = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
TILO_ROOT_TYPE = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab"
CODE = ["/usr/share/edk2/aarch64/QEMU_EFI-silent-pflash.raw", "/usr/share/edk2/aarch64/QEMU_EFI-pflash.raw",
        "/usr/share/AAVMF/AAVMF_CODE.fd"]
VARS = ["/usr/share/edk2/aarch64/vars-template-pflash.raw", "/usr/share/AAVMF/AAVMF_VARS.fd"]
# The board's ramoops layout (os/hardware: 83ED), at an address inside this VM's RAM.
RAMOOPS = """
\treserved-memory {
\t\t#address-cells = <2>;
\t\t#size-cells = <2>;
\t\tranges;
\t\tramoops@f0000000 {
\t\t\tcompatible = "ramoops";
\t\t\treg = <0x0 0xf0000000 0x0 0x200000>;
\t\t\tno-map;
\t\t\tconsole-size = <0x100000>;
\t\t\trecord-size = <0x40000>;
\t\t\tecc-size = <16>;
\t\t};
\t};
"""

uki, work = Path(sys.argv[1]), Path(sys.argv[2])
shutil.rmtree(work, ignore_errors=True)
work.mkdir(parents=True)


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def first(paths):
    for path in paths:
        if os.path.exists(path):
            return path
    sys.exit(f"none of {paths} exists")


code = first(CODE)
vars_file = work / "vars.fd"
shutil.copyfile(first(VARS), vars_file)
disk = work / "disk.raw"
serial = work / "serial.log"
monitor_path = work / "monitor.sock"
machine = [
    "qemu-system-aarch64", "-accel", "kvm", "-cpu", "host", "-smp", "4", "-m", "3072",
    "-drive", f"if=pflash,format=raw,readonly=on,file={code}",
    "-drive", f"if=pflash,format=raw,file={vars_file}",
    "-drive", f"file={disk},format=raw,if=virtio",
    "-nic", "none", "-display", "none",
    "-device", "virtio-gpu-pci", "-device", "qemu-xhci", "-device", "usb-kbd",
]

# QEMU virt's device tree for exactly this machine, with the ramoops region added.
with open(disk, "wb") as handle:
    handle.truncate(1 << 20)
run(*machine, "-machine", f"virt,gic-version=max,dumpdtb={work / 'virt.dtb'}", capture_output=True)
dts = run("dtc", "-q", "-I", "dtb", "-O", "dts", str(work / "virt.dtb"), capture_output=True, text=True).stdout
assert "reserved-memory" not in dts, "QEMU virt already has a reserved-memory node"
dts = dts.replace("\n};", RAMOOPS + "};", 1) if dts.rstrip().endswith("};") else None
assert dts, "unexpected device tree source"
(work / "vm.dts").write_text(dts)
dtb = run("dtc", "-q", "-I", "dts", "-O", "dtb", str(work / "vm.dts"), capture_output=True).stdout

# The UKI with its .dtb section's content replaced in place: same stub, same command line,
# same initramfs. Secure Boot is off in this firmware, so the changed bytes are not checked.
data = bytearray(uki.read_bytes())
pe = struct.unpack_from("<I", data, 0x3c)[0]
sections = struct.unpack_from("<H", data, pe + 6)[0]
table = pe + 24 + struct.unpack_from("<H", data, pe + 20)[0]
replaced = []
for i in range(sections):
    at = table + 40 * i
    name, vsize, vaddr, rawsize, rawptr = struct.unpack_from("<8sIIII", data, at)
    name = name.rstrip(b"\0").decode()
    if name in (".dtb", ".dtbauto"):
        assert len(dtb) <= rawsize, f"the VM device tree ({len(dtb)}) does not fit {name} ({rawsize})"
        data[rawptr:rawptr + rawsize] = dtb + bytes(rawsize - len(dtb))
        struct.pack_into("<I", data, at + 8, len(dtb))
        replaced.append(name)
assert replaced, "the board UKI carries no device tree"
vm_uki = work / "BOOTAA64.EFI"
vm_uki.write_bytes(data)
print(f"replaced the UKI's {', '.join(replaced)} with QEMU virt's device tree plus ramoops", flush=True)

# An ESP with the UKI as the removable-media loader, then a LUKS2 Tilo root.
esp, root = work / "esp.img", work / "root.img"
esp_mib, root_mib = len(data) // (1 << 20) + 64, 64
with open(esp, "wb") as handle:
    handle.truncate(esp_mib << 20)
run("mkfs.vfat", "-F", "32", str(esp), capture_output=True)
run("mmd", "-i", str(esp), "::/EFI", "::/EFI/BOOT")
run("mcopy", "-i", str(esp), str(vm_uki), "::/EFI/BOOT/BOOTAA64.EFI")
with open(root, "wb") as handle:
    handle.truncate(root_mib << 20)
run("cryptsetup", "luksFormat", "--batch-mode", "--disable-locks", "--type", "luks2",
    "--pbkdf", "pbkdf2", "--pbkdf-force-iterations", "1000", "--key-file", "-", str(root),
    input=PASSPHRASE.encode())
with open(disk, "wb") as handle:
    handle.truncate((1 + esp_mib + root_mib + 1) << 20)
run("sfdisk", "-q", str(disk), input=(f"label: gpt\nstart=2048, size={esp_mib}M, type={ESP_TYPE}\n"
                                      f"size={root_mib}M, type={TILO_ROOT_TYPE}\n").encode())
run("dd", f"if={esp}", f"of={disk}", "bs=1M", "seek=1", "conv=notrunc", "status=none")
run("dd", f"if={root}", f"of={disk}", "bs=1M", f"seek={1 + esp_mib}", "conv=notrunc", "status=none")
esp.unlink(); root.unlink()

qemu = subprocess.Popen(machine + ["-machine", "virt,gic-version=max",
                                   "-serial", f"file:{serial}",
                                   "-monitor", f"unix:{monitor_path},server,nowait"])


class Monitor:
    def __init__(self):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(30)
        for _ in range(100):
            try:
                self.sock.connect(str(monitor_path)); break
            except OSError:
                time.sleep(0.1)
        self.read()

    def read(self):
        data = b""
        while not data.endswith(b"(qemu) "):
            chunk = self.sock.recv(65536)
            if not chunk: break
            data += chunk
        return data.decode(errors="replace")

    def command(self, line):
        self.sock.sendall(line.encode() + b"\n")
        return self.read()


def log_text():
    return serial.read_text(errors="replace") if serial.exists() else ""


def wait_for(pattern, timeout, after=0):
    end = time.monotonic() + timeout
    while time.monotonic() < end and qemu.poll() is None:
        match = re.search(pattern, log_text()[after:])
        if match:
            return match
        time.sleep(1)
    return None


def type_line(monitor, text):
    for char in text:
        monitor.command("sendkey " + {"-": "minus"}.get(char, char))
    monitor.command("sendkey ret")


result = {}
try:
    monitor = Monitor()
    # The prompt: systemd-cryptsetup asks once tilo-root-discover found the LUKS root.
    asked = wait_for(r"(?i)Please enter passphrase|Starting.*tilo-root-discover|Discover the Tilo root", 600)
    result["prompt"] = bool(asked)
    time.sleep(20)  # the agent and Plymouth's prompt come up after the request
    monitor.command(f"screendump {work / 'prompt.ppm'}")
    mark = len(log_text())
    type_line(monitor, WRONG)
    result["wrong passphrase rejected"] = bool(
        wait_for(r"Passphrase incorrect|Failed to activate with specified passphrase", 120, mark))
    time.sleep(5)
    mark = len(log_text())
    type_line(monitor, PASSPHRASE)
    result["right passphrase opened the root"] = bool(
        wait_for(r"Finished.*(tilo-root-discover|Discover the Tilo root)|Failed to mount.*[Ss]ysroot", 180, mark))
    time.sleep(10)
    monitor.command(f"screendump {work / 'after.ppm'}")
finally:
    try:
        Monitor().command("quit")
    except Exception:
        pass
    try:
        qemu.wait(30)
    except subprocess.TimeoutExpired:
        qemu.kill()
    disk.unlink(missing_ok=True)

text = log_text()
print("== serial (tail)")
print(text[-12000:])
checks = {
    # Info-level journal lines on the serial console need both journald's forwarding to the
    # kernel log and the console log level unit, which runs only when ramoops bound: the
    # stub installed the replaced device tree.
    "the initrd's journal reached the console": bool(re.search(r"systemd\[1\]: (Starting|Started|Finished|Reached)", text)),
    "the unlock prompt came up": result.get("prompt", False),
    "the typed wrong passphrase reached the agent and was rejected": result.get("wrong passphrase rejected", False),
    "the typed passphrase opened the root": result.get("right passphrase opened the root", False),
}
for what, ok in checks.items():
    print(("ok   " if ok else "FAIL ") + what)
sys.exit(0 if all(checks.values()) else 1)
