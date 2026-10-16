#!/usr/bin/env python3
"""Boot stock Fedora under KVM, log into the packaged Tilo session, open apps.

The host-desktop counterpart of the Tilo OS acceptance suite: the
tilo-session RPMs CI built, installed on the Fedora Cloud image with dnf, a
gnome-keyring Secret Service unlocked on the user's bus, the identity
provisioned there, GNOME input sources seeded the way a GNOME user has them
(German without dead keys, Compose on Right Alt), SDDM autologin into /usr/share/wayland-sessions/tilo.desktop,
then each app opened the way a person opens it (Super+Space, its name, Enter,
typed on the VM's keyboard through QMP). The session must have imported the
GNOME input sources on its first login, and the lock screen password and app
names are typed on the German layout it imported. After each app starts, the compositor
must still be the same process for the whole dwell, with no crash record,
core dump, unexpected session exit or panic.

    run.py test --rpms DIR --out DIR        the VM test; exit 0 only on pass

DIR holds the fedora-44 RPMs (`dist/ci/run-package-job.sh fedora-44 ...`
writes them to dist/ci-output/fedora-44-amd64) and, optionally, a
source.json describing where they came from.

The production session has no control socket, so the run drives it only
through the VM's keyboard, SSH as root and screenshots (QMP screendump).
`dist/session/vm-e2e/guest.sh` does the guest side. CI only: it needs /dev/kvm
and starts a display manager inside the VM it creates.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
# Fedora 44 Cloud Base (Generic), the release image: the stock Fedora the
# fedora-44 RPM lane builds for. Pinned by the CHECKSUM Fedora publishes.
IMAGE_URL = ("https://download.fedoraproject.org/pub/fedora/linux/releases/44/Cloud/"
             "x86_64/images/Fedora-Cloud-Base-Generic-44-1.7.x86_64.qcow2")
IMAGE_SHA256 = "28680fe5b371a5a82ebf43a31926e086a168e59949d03969c5093e7071f90b7f"
# Name typed into search, and the package id its launcher process carries.
APPS = [("Settings", "so.tilo.settings"), ("Notes", "so.tilo.notes"),
        ("Files", "so.tilo.file-explorer"), ("Terminal", "so.tilo.terminal")]
SSH_PORT = 2222
# QEMU ids of the VM's two KMS cards; card0 is the boot display.
GPUS = ("gpu0", "gpu1")
GUEST_USER = "fedora"
# The throwaway guest account's fixture password, also set by guest.sh.
GUEST_PASSWORD = "tilo-e2e-password"
# What the session's first-login import must make of the GNOME settings
# guest.sh seeds (sources [('xkb', 'de+nodeadkeys')], xkb-options
# ['compose:ralt']): crates/session/shell/src/firstboot.rs.
IMPORTED_INPUT = {"keyboard_layout": "de:nodeadkeys", "keyboard_layouts": ["de:nodeadkeys"],
                  "compose_key": "ralt"}
# Written once the import has completed; its absence means it never ran.
IMPORT_MARKER = "host_input_migration_version"
# QMP sends key positions. Where a layout puts a character on another key
# than US does, the key that types it there.
LAYOUT_KEYS = {
    "us": {"-": "minus"},
    "de": {"-": "slash", "y": "z", "z": "y"},
}


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


class Fail(Exception):
    pass


# -- the VM ----------------------------------------------------------------------

class Qmp:
    def __init__(self, path: Path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(60)
        self.sock.connect(str(path))
        self.buf = b""
        self._read()  # greeting
        self.call("qmp_capabilities")

    def _read(self) -> dict:
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise Fail("QMP connection closed")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def call(self, command: str, **arguments) -> dict:
        self.sock.sendall(json.dumps({"execute": command, "arguments": arguments}).encode() + b"\n")
        while True:
            msg = self._read()
            if "event" in msg:
                continue
            if "error" in msg:
                raise Fail(f"QMP {command}: {msg['error']}")
            return msg.get("return", {})

    def keys(self, *qcodes: str) -> None:
        self.call("send-key", keys=[{"type": "qcode", "data": q} for q in qcodes], **{"hold-time": 80})
        time.sleep(0.15)

    def type_text(self, text: str, layout: str = "us") -> None:
        for keys in key_presses(text, layout):
            self.keys(*keys)

    def screendump(self, path: Path, device: str | None = None) -> None:
        """The display of `device` (a QEMU device id), or the first one."""
        extra = {"device": device} if device else {}
        self.call("screendump", filename=str(path), format="png", **extra)


def key_presses(text: str, layout: str = "us") -> list[tuple[str, ...]]:
    """The QMP key presses that type `text` on the session's `layout`."""
    keys = LAYOUT_KEYS[layout]
    presses = []
    for ch in text:
        if ch == " ":
            presses.append(("spc",))
        elif ch.isupper():
            presses.append(("shift", keys.get(ch.lower(), ch.lower())))
        else:
            presses.append((keys.get(ch, ch),))
    return presses


def typing_layout(settings: dict) -> str:
    """The layout to type on: the one the session's settings select."""
    layout = ((settings.get("input") or {}).get("keyboard_layout") or "us")
    return "de" if str(layout).split(":", 1)[0] == "de" else "us"


def input_import_problems(settings_text: str) -> tuple[dict, list[str]]:
    """(settings, problems): whether the first login imported the GNOME input settings."""
    if not settings_text.strip():
        return {}, ["the session wrote no settings.json: the GNOME input settings were not imported"]
    try:
        settings = json.loads(settings_text)
    except json.JSONDecodeError as error:
        return {}, [f"the session's settings.json is not JSON ({error}): {settings_text[:200]!r}"]
    if not isinstance(settings, dict):
        return {}, [f"the session's settings.json is not an object: {settings_text[:200]!r}"]
    section = settings.get("input")
    if not isinstance(section, dict):
        return settings, ["the session's settings.json has no input section: the GNOME input "
                          "settings were not imported"]
    problems = [f"input.{key} is {section.get(key)!r}, want {want!r}"
                for key, want in IMPORTED_INPUT.items() if section.get(key) != want]
    marker = section.get(IMPORT_MARKER)
    if not isinstance(marker, int) or marker < 1:
        problems.append(f"input.{IMPORT_MARKER} is {marker!r}: the first-login import of the host's "
                        "GNOME input settings did not complete")
    return settings, problems


class Vm:
    def __init__(self, work: Path, image: Path, memory_mb: int, cpus: int):
        self.work = work
        self.key = work / "id_ed25519"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(self.key)], check=True)
        self._seed()
        self.disk = work / "overlay.qcow2"
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-F", "qcow2", "-b", str(image),
                        str(self.disk), "40G"], check=True)
        self.qmp_path = work / "qmp.sock"
        self.serial = work / "serial.log"
        self.qemu = subprocess.Popen([
            "qemu-system-x86_64", "-machine", "q35,accel=kvm", "-cpu", "host",
            "-smp", str(cpus), "-m", str(memory_mb),
            "-drive", f"file={self.disk},if=virtio,format=qcow2",
            "-drive", f"file={work / 'seed.iso'},if=virtio,format=raw,readonly=on",
            "-nic", f"user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:{SSH_PORT}-:22",
            # No virgl: the runners have no GPU, so Tilo renders with lavapipe
            # onto virtio-gpu's KMS scanout, as in the Tilo OS acceptance VMs.
            # Two cards, each with its own connector: the session must light
            # both, as on a laptop whose panel and HDMI port hang off
            # different GPUs.
            "-vga", "none", "-device", f"virtio-vga,id={GPUS[0]}",
            "-device", f"virtio-gpu-pci,id={GPUS[1]}",
            "-device", "virtio-keyboard-pci", "-device", "virtio-tablet-pci",
            "-display", "none", "-serial", f"file:{self.serial}", "-monitor", "none",
            "-qmp", f"unix:{self.qmp_path},server=on,wait=off",
        ])
        for _ in range(100):
            if self.qmp_path.exists() or self.qemu.poll() is not None:
                break
            time.sleep(0.1)
        if self.qemu.poll() is not None:
            raise Fail(f"QEMU exited at start with status {self.qemu.returncode}")
        self.qmp = Qmp(self.qmp_path)

    def _seed(self) -> None:
        seed = self.work / "seed"
        seed.mkdir()
        pub = (self.work / "id_ed25519.pub").read_text().strip()
        (seed / "meta-data").write_text("instance-id: tilo-host-session-e2e\nlocal-hostname: tilo-e2e\n")
        (seed / "user-data").write_text(
            "#cloud-config\n"
            f"ssh_authorized_keys:\n  - {pub}\n"
            "write_files:\n"
            "  - path: /etc/tilo-host-session-e2e\n"
            "    content: 'throwaway VM of dist/session/vm-e2e/run.py'\n"
            # Fedora's dnf is slow to pick mirrors from a fresh cache.
            "  - path: /etc/dnf/libdnf5.conf.d/90-tilo-e2e.conf\n"
            "    content: |\n      [main]\n      max_parallel_downloads=10\n")
        subprocess.run(["xorriso", "-as", "genisoimage", "-quiet", "-output", str(self.work / "seed.iso"),
                        "-volid", "cidata", "-joliet", "-rock", str(seed / "user-data"),
                        str(seed / "meta-data")], check=True)

    def alive(self) -> bool:
        return self.qemu.poll() is None

    def _ssh_args(self) -> list[str]:
        return ["-i", str(self.key), "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                "-o", "LogLevel=ERROR", "-o", "ConnectTimeout=10", "-o", "BatchMode=yes"]

    def ssh(self, command: str, timeout: float = 600, check: bool = True,
            capture: bool = False) -> subprocess.CompletedProcess:
        if not self.alive():
            raise Fail("QEMU exited")
        result = subprocess.run(["ssh", *self._ssh_args(), "-p", str(SSH_PORT),
                                 f"{GUEST_USER}@127.0.0.1", command],
                                timeout=timeout, text=True,
                                stdout=subprocess.PIPE if capture else None,
                                stderr=subprocess.STDOUT if capture else None)
        if check and result.returncode != 0:
            raise Fail(f"guest command failed ({result.returncode}): {command}"
                       + (f"\n{result.stdout}" if capture else ""))
        return result

    def scp_to(self, sources: list[Path], dest: str) -> None:
        subprocess.run(["scp", *self._ssh_args(), "-P", str(SSH_PORT), *map(str, sources),
                        f"{GUEST_USER}@127.0.0.1:{dest}"], check=True, timeout=600)

    def scp_from(self, source: str, dest: Path) -> None:
        subprocess.run(["scp", *self._ssh_args(), "-r", "-P", str(SSH_PORT),
                        f"{GUEST_USER}@127.0.0.1:{source}", str(dest)], check=False, timeout=600)

    def wait_ssh(self, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.alive():
                raise Fail("QEMU exited while booting")
            if self.ssh("true", timeout=30, check=False, capture=True).returncode == 0:
                return
            time.sleep(3)
        raise Fail(f"no SSH within {timeout:.0f}s")

    def guest(self, *args: str, timeout: float = 1800, check: bool = True,
              capture: bool = False) -> subprocess.CompletedProcess:
        return self.ssh("sudo -n bash /var/tmp/tilo-e2e/guest.sh " + " ".join(args),
                        timeout=timeout, check=check, capture=capture)

    def status(self, apps: list[str]) -> dict:
        out = self.guest("status", *apps, timeout=60, capture=True).stdout
        return dict(line.split("=", 1) for line in out.splitlines() if "=" in line)

    def settings(self) -> str:
        return self.guest("settings", timeout=60, capture=True).stdout

    def reboot(self) -> None:
        self.ssh("sudo -n systemctl reboot", timeout=30, check=False, capture=True)
        time.sleep(10)
        self.wait_ssh(300)

    def stop(self) -> None:
        if self.alive():
            try:
                self.qmp.call("quit")
            except (Fail, OSError):
                pass
            try:
                self.qemu.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.qemu.kill()


def fetch_image(cache: Path) -> Path:
    cache.mkdir(parents=True, exist_ok=True)
    image = cache / Path(IMAGE_URL).name
    if image.exists() and _sha256(image) == IMAGE_SHA256:
        return image
    log(f"downloading {IMAGE_URL}")
    partial = image.with_suffix(".part")
    subprocess.run(["curl", "-fsSL", "--retry", "5", "--retry-all-errors", "-o", str(partial), IMAGE_URL],
                   check=True)
    digest = _sha256(partial)
    if digest != IMAGE_SHA256:
        raise Fail(f"{image.name}: sha256 {digest}, want {IMAGE_SHA256}")
    partial.rename(image)
    return image


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


# -- the test --------------------------------------------------------------------

class Report:
    def __init__(self, out: Path, meta: dict):
        self.out = out
        self.meta = meta
        self.checks: list[dict] = []

    def add(self, cid: str, title: str, ok: bool, detail: str = "", **numbers) -> bool:
        self.checks.append({"id": cid, "title": title, "status": "pass" if ok else "fail",
                            "detail": detail, "numbers": numbers})
        log(f"{cid} {'PASS' if ok else 'FAIL'} {title}" + (f": {detail}" if detail else ""))
        return ok

    def write(self) -> bool:
        overall = "pass" if self.checks and all(c["status"] == "pass" for c in self.checks) else "fail"
        doc = {**self.meta, "overall": overall, "checks": self.checks}
        (self.out / "report.json").write_text(json.dumps(doc, indent=1) + "\n")
        src = self.meta.get("rpm_source") or {}
        lines = [f"# Tilo host-desktop session on Fedora 44 — **{overall.upper()}**", "",
                 "| | |", "|---|---|",
                 f"| RPMs | {', '.join(src.get('rpms', []))} |",
                 f"| Built by | {src.get('workflow', 'run')} #{src.get('run')} at `{src.get('commit')}` |",
                 f"| Guest | {Path(IMAGE_URL).name}, {self.meta['memory_mb']} MiB, {self.meta['cpus']} vCPU, "
                 "two virtio-gpu cards without virgl |",
                 "| Session | SDDM autologin into tilo.desktop, gnome-keyring Secret Service unlocked |",
                 f"| Dwell after each app | {self.meta['dwell']} s |", "",
                 "| Check | Result | Detail | Numbers |", "|---|---|---|---|"]
        for c in self.checks:
            detail = c["detail"].replace("|", "\\|").replace("\n", " ")
            numbers = json.dumps(c["numbers"]).replace("|", "\\|") if c["numbers"] else ""
            mark = "pass" if c["status"] == "pass" else "**FAIL**"
            lines.append(f"| {c['id']} {c['title']} | {mark} | {detail} | {numbers} |")
        (self.out / "report.md").write_text("\n".join(lines) + "\n")
        return overall == "pass"


def compositor_health(st: dict, pid: str) -> list[str]:
    problems = []
    if st.get("compositor_pid") != pid:
        problems.append(f"compositor pid {pid} is gone (now: {st.get('compositor_pid') or 'none'})")
    if st.get("crash_message"):
        problems.append(f"crash: {st['crash_message']}")
    for key, what in (("crash_logs", "crash log(s) in ~/.cache/tilo/crashes"),
                      ("coredumps", "tilo-shell core dump(s)"),
                      ("unexpected_exit", "'compositor exited without a logout request' in the journal"),
                      ("panics", "panic line(s) in the journal or session log")):
        if int(st.get(key) or 0):
            problems.append(f"{st[key]} {what}")
    return problems


def run_test(args) -> int:
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    shots = out / "shots"
    shots.mkdir(exist_ok=True)
    rpms = sorted(Path(args.rpms).glob("*.rpm"))
    source_file = Path(args.rpms) / "source.json"
    meta = {"rpm_source": json.loads(source_file.read_text()) if source_file.exists()
            else {"rpms": [p.name for p in rpms]},
            "image": IMAGE_URL, "image_sha256": IMAGE_SHA256, "memory_mb": args.memory,
            "cpus": args.cpus, "dwell": args.dwell, "started": time.strftime("%Y-%m-%dT%H:%M:%S%z")}
    report = Report(out, meta)
    install = [p for p in rpms if not p.name.startswith("tilo-session-devel")]
    if len(install) != 2:
        raise Fail(f"want tilo-session and tilo-session-apps in {args.rpms}, found {[p.name for p in rpms]}")
    work = Path(args.work).resolve()
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    image = fetch_image(Path(args.image_cache).resolve())
    apps = [a for a in APPS if not args.apps or a[1] in args.apps.split(",")]

    vm = None
    try:
        vm = Vm(work, image, args.memory, args.cpus)
        log("booting Fedora 44 Cloud")
        vm.wait_ssh(600)
        vm.ssh("mkdir -p /var/tmp/tilo-e2e/rpms")
        vm.scp_to([HERE / "guest.sh"], "/var/tmp/tilo-e2e/")
        vm.scp_to(install, "/var/tmp/tilo-e2e/rpms/")

        log("installing the RPMs, SDDM and gnome-keyring with dnf")
        result = vm.guest("install", "/var/tmp/tilo-e2e/rpms", check=False)
        if result.returncode == 100:
            log("rebooting into the kernel whose DRM modules were installed")
            vm.reboot()
        elif result.returncode != 0:
            raise Fail("installing the packages failed")
        vm.guest("user")
        seeded = vm.guest("gnome-input", check=False, capture=True)
        (out / "gnome-input.log").write_text(seeded.stdout or "")
        if seeded.returncode != 0:
            report.add("H05", "The first login imports the host's GNOME input settings", False,
                       "seeding the GNOME input settings failed: "
                       + ((seeded.stdout or "").strip().splitlines() or ["no output"])[-1])
            return 1
        keyring = vm.guest("keyring", check=False, capture=True)
        if not report.add("H01", "gnome-keyring answers org.freedesktop.secrets, unlocked",
                          keyring.returncode == 0, keyring.stdout.strip().splitlines()[-1] if keyring.stdout else ""):
            return 1
        identity = vm.guest("identity", check=False, capture=True)
        (out / "provision.log").write_text(identity.stdout or "")
        if not report.add("H02", "The identity is provisioned with its secret in the Secret Service",
                          identity.returncode == 0,
                          (identity.stdout or "").strip().splitlines()[-1] if identity.stdout else ""):
            return 1

        log("logging in through SDDM autologin into tilo.desktop")
        vm.guest("login")
        pid, st = "", {}
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            st = vm.status([])
            if st.get("compositor_pid") and int(st.get("modeset") or 0):
                pid = st["compositor_pid"]
                break
            time.sleep(3)
        if not report.add("H03", "The display manager starts the Tilo compositor on a DRM output",
                          bool(pid), "" if pid else f"no compositor with a modeset within 240 s: {st}",
                          compositor_pid=pid):
            vm.qmp.screendump(shots / "login.png")
            return 1
        # The desktop is ready for input once peer sync and the desktop
        # chrome are up; give a software renderer time for its first frames.
        time.sleep(args.settle)
        vm.qmp.screendump(shots / "00-desktop.png")
        vm.qmp.screendump(shots / "00-desktop-second-gpu.png", GPUS[1])
        st = vm.status([])
        problems = compositor_health(st, pid)
        if not report.add("H04", f"The session survives {args.settle} s idle after login", not problems,
                          "; ".join(problems)):
            return 1
        # The compositor journals what it drives whenever that changes; the
        # last line is the topology now.
        driven = int(st.get("driven_cards") or 0)
        report.add("H06", "The compositor drives an output on each of the VM's two GPUs",
                   driven == len(GPUS), st.get("topology") or "no DRM topology line in the session log",
                   driven_cards=driven)

        # The import runs before the compositor starts; the keymap is built
        # from its result, so everything typed below goes through it.
        settings, problems = input_import_problems(vm.settings())
        report.add("H05", "The first login imports the host's GNOME input settings", not problems,
                   "; ".join(problems), **{key: (settings.get("input") or {}).get(key)
                                            for key in (*IMPORTED_INPUT, IMPORT_MARKER)})
        # Type on the layout the session actually selected, so a broken import
        # fails H05 rather than every later check.
        layout = typing_layout(settings)

        # Autologin starts the real compositor locked. Authenticate through
        # its normal PAM-backed lock screen before sending app search input.
        # Keep lock policy and failed-authentication behavior intact.
        log("authenticating the throwaway guest account at the lock screen")
        vm.qmp.type_text(GUEST_PASSWORD, layout)
        vm.qmp.keys("ret")
        time.sleep(2)

        for n, (name, app_id) in enumerate(apps, start=1):
            cid = f"A{n:02d}"
            log(f"opening {name} through search")
            vm.qmp.keys("esc")
            vm.qmp.keys("meta_l", "spc")
            time.sleep(1.5)
            vm.qmp.type_text(name, layout)
            time.sleep(1.5)
            vm.qmp.screendump(shots / f"{n:02d}-{app_id}-search.png")
            vm.qmp.keys("ret")
            title = f"{name} opens and the compositor survives {args.dwell} s"
            # The launch shows in the session log as the app's own output.
            started, st = None, {}
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                st = vm.status([app_id])
                if int(st.get(f"app.{app_id}") or 0):
                    started = time.monotonic()
                    break
                if compositor_health(st, pid):
                    break
                time.sleep(1)
            if started is None:
                problems = compositor_health(st, pid)
                vm.qmp.screendump(shots / f"{n:02d}-{app_id}-missing.png")
                report.add(cid, title, False,
                           "; ".join(problems) if problems else f"{name} ({app_id}) did not start within 60 s")
                if problems:
                    return 1
                continue
            # The app's first frame, its broker connection and its presence
            # publication all happen in this window.
            survived, problems = 0.0, []
            while time.monotonic() - started < args.dwell:
                time.sleep(2)
                st = vm.status([app_id])
                problems = compositor_health(st, pid)
                if problems:
                    break
                survived = time.monotonic() - started
            vm.qmp.screendump(shots / f"{n:02d}-{app_id}.png")
            if int(st.get(f"app_stopped.{app_id}") or 0):
                # Not what this test guards, but an app that never shows a
                # window cannot be said to have opened.
                problems.append(f"{name} stopped before presenting a window: "
                                f"{st.get(f'app_error.{app_id}') or 'no error line'}")
            report.add(cid, title, not problems, "; ".join(problems), app_id=app_id,
                       survived_s=round(survived, 1))
            if st.get("compositor_pid") != pid:
                return 1
        return 0 if all(check["status"] == "pass" for check in report.checks) else 1
    except (Fail, subprocess.SubprocessError, OSError) as error:
        report.add("H00", "The test ran to completion", False, str(error))
        return 1
    finally:
        if vm is not None:
            try:
                if vm.alive():
                    vm.guest("collect", "/var/tmp/tilo-e2e/evidence", check=False, timeout=300)
                    vm.scp_from("/var/tmp/tilo-e2e/evidence", out)
            except (Fail, subprocess.SubprocessError, OSError) as error:
                log(f"collecting guest evidence failed: {error}")
            vm.stop()
        serial = work / "serial.log"
        if serial.exists():
            shutil.copy(serial, out / "serial.log")
        meta["finished"] = time.strftime("%Y-%m-%dT%H:%M:%S%z")
        report.write()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    test = sub.add_parser("test")
    test.add_argument("--rpms", required=True)
    test.add_argument("--out", required=True)
    test.add_argument("--work", default=os.environ.get("RUNNER_TEMP", "/var/tmp") + "/tilo-host-session-e2e")
    test.add_argument("--image-cache", default="/var/tmp/tilo-host-session-e2e-images")
    test.add_argument("--apps", default="", help="comma-separated package ids (default: all)")
    test.add_argument("--memory", type=int, default=6144)
    test.add_argument("--cpus", type=int, default=4)
    test.add_argument("--settle", type=int, default=30)
    test.add_argument("--dwell", type=int, default=45)
    args = parser.parse_args()
    try:
        return run_test(args)
    except Fail as error:
        print(f"host-session e2e: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
