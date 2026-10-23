#!/usr/bin/env python3
"""A board image's initrd failure survives a warm reset in the pstore console.

    board_initrd_pstore_vm.py WORKDIR

WORKDIR holds the UKI's sections (linux, initrd, cmdline). The kernel boots
under KVM on QEMU's virt board with the UKI's command line, minus its root, so
the initrd waits for a root that never appears and enters emergency mode. A
ramoops region of the 83ED's layout is given by parameter, since virt has no
83ED device tree. A debug shell on the serial port reads the state; QEMU's
system_reset is the warm reset, which keeps guest RAM. The next boot must find
the failed boot's console record in /sys/fs/pstore with the initrd's failure
in it. Exits nonzero when it does not.
"""
import os, re, socket, subprocess, sys, time

work = sys.argv[1]
cmdline = open(f"{work}/cmdline").read().strip().replace("\0", "")
# No disk: the root never appears and the initrd enters emergency mode.
args = [a for a in cmdline.split() if not a.startswith(("root=", "composefs="))]
args += [
    "root=PARTUUID=00000000-0000-0000-0000-00000000dead",
    "systemd.default_device_timeout_sec=30",
    # QEMU's virt board has no 83ED device tree: the same ramoops layout by parameter,
    # in RAM mem= leaves to it.
    "mem=2816M", "ramoops.mem_address=0xf0000000", "ramoops.mem_size=0x200000",
    "ramoops.console_size=0x100000", "ramoops.record_size=0x40000", "ramoops.ecc=16",
    # A root shell on the serial port to read the record with; the emergency shell
    # itself is locked.
    "rd.systemd.debug_shell=/dev/ttyAMA0",
]
ser, mon = f"{work}/serial.sock", f"{work}/monitor.sock"
for p in (ser, mon):
    if os.path.exists(p): os.unlink(p)
log = open(f"{work}/serial.log", "wb")
qemu = subprocess.Popen([
    "qemu-system-aarch64", "-M", "virt,gic-version=max", "-accel", "kvm", "-cpu", "host",
    "-smp", "4", "-m", "3G",
    "-display", "none", "-nodefaults",
    "-kernel", f"{work}/linux", "-initrd", f"{work}/initrd", "-append", " ".join(args),
    "-chardev", f"socket,id=s0,path={ser},server=on,wait=off", "-serial", "chardev:s0",
    "-monitor", f"unix:{mon},server,nowait",
])
for _ in range(100):
    if os.path.exists(ser) and os.path.exists(mon): break
    time.sleep(0.2)
s = socket.socket(socket.AF_UNIX); s.connect(ser); s.settimeout(1)
buf = bytearray()

def read_until(pattern, timeout):
    end = time.time() + timeout
    while time.time() < end:
        try:
            chunk = s.recv(65536)
        except socket.timeout:
            chunk = b""
        if chunk:
            buf.extend(chunk); log.write(chunk); log.flush()
        if re.search(pattern, buf):
            return True
    return False

def send(text):
    s.sendall(text.encode())

def monitor(command):
    m = socket.socket(socket.AF_UNIX); m.connect(mon); time.sleep(0.3)
    m.sendall((command + "\n").encode()); time.sleep(1); m.close()

def run(command, timeout=60):
    """A command's output in the debug shell, between two markers."""
    del buf[:]
    send(f"echo @@S@@; {command}; echo @@E@@\n")
    read_until(rb"\n@@E@@", timeout)
    text = bytes(buf).decode(errors="replace").replace("\r", "")
    try:
        return text.split("\n@@S@@\n", 1)[1].split("\n@@E@@", 1)[0]
    except IndexError:
        return text

result = {}
try:
    # Boot 1: the debug shell comes up early; wait for the initrd to give up on the root.
    ok = read_until(rb"sh-[0-9.]+#", 600)
    result["boot1_shell"] = ok
    # dracut's emergency shell, not emergency.target: wait for its banner.
    result["boot1_emergency"] = read_until(rb"[Ee]mergency mode", 600)
    time.sleep(10)
    result["boot1_state"] = run("systemctl is-active tilo-pstore-console.service; cat /proc/sys/kernel/printk; "
                                "cat /proc/cmdline; journalctl -b -p err --no-pager | tail -n 8")
    time.sleep(5)
    monitor(f"pmemsave 0xf0000000 0x200000 {work}/ramoops-boot1.bin")
    # Warm reset: QEMU keeps guest RAM.
    monitor("system_reset")
    del buf[:]
    result["boot2_shell"] = read_until(rb"sh-[0-9.]+#", 600)
    time.sleep(5)
    result["boot2_pstore"] = run("ls -l /sys/fs/pstore")
    result["boot2_record"] = run("wc -l /sys/fs/pstore/console-ramoops-0; grep -E -i "
                                 "'emergency|timed out|dependency failed|failed' "
                                 "/sys/fs/pstore/console-ramoops-0 | head -n 20", 120)
finally:
    monitor("quit")
    qemu.wait(30)
for k, v in result.items():
    print(f"== {k}\n{v}\n")
record = result.get("boot2_record", "")
checks = {
    "the initrd entered emergency mode": result.get("boot1_emergency") is True,
    "the console log level unit ran in the initrd": result.get("boot1_state", "").lstrip().startswith("active"),
    "the console log level was 7 in the initrd": "\n7\t" in "\n" + result.get("boot1_state", "").split("\n", 1)[-1],
    "the next boot has the failed boot's console record": "console-ramoops-0" in result.get("boot2_pstore", ""),
    "the record carries the initrd's failure": bool(re.search(r"(?i)timed out|dependency failed|emergency", record)),
}
failed = [what for what, ok in checks.items() if not ok]
for what, ok in checks.items():
    print(("ok   " if ok else "FAIL ") + what)
sys.exit(1 if failed else 0)
