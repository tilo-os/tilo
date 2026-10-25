#!/usr/bin/env python3
"""One-secret sign-in, end to end, on a fresh encrypted install in a KVM guest with a software TPM.

  signin_e2e.py --medium INSTALLER_QCOW2 --out DIR --storage DIR [--variant one-secret|separate-disk-secret]...

The installer medium is dev-installer-medium-x86_64's. Everything runs through the repository's
VM harness: os/dev/vm.sh boots the live installer and the installed target in one named cycle
(disk, swtpm state and firmware persist across its boots and nowhere else), and the installer
and greeter are driven through their console channel (os/dev/installer-console.py, the same
actions tilo-vm scenario install and the release gate use). Early boot has no channel but the
keyboard and the screen: secrets are typed with QEMU key events and the boot screens are read
from the framebuffer (software graphics, OCR).

Guest inspection (the person's record, the volume's keyslots and tokens, the shadow entry, the
TPM's lockout counter, sudo/polkit/lock-screen checks) needs a root shell the image does not
ship. The live installer has one (its tty2 recovery tools). From it this check writes a test-only
unit into the installed target's /etc (this throwaway disk only, never the image) that serves a
root shell on a virtio-serial port the check hot-plugs after each boot. Nothing else on the
target is changed.

Variants:
  one-secret             the installer's own path: one secret for disk and account
  separate-disk-secret   the installer's separate disk secret: migration adds the keyslot with the
                         volume key the initrd links, then the boot sign-in takes the password

Writes report.json and report.md, screenshots and per-boot logs into --out. Every file there is
searched for every secret the run used before the run ends; a file that holds one is emptied
and the run fails.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
VM_SH = REPO / "os/dev/vm.sh"
TILO_ROOT_TYPE = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab"
SHELL_PORT = "org.tilo.test-shell.0"
RECOVERY_SHAPE = re.compile(r"\b[cbdefghijklnrtuv]{8}(?:-[cbdefghijklnrtuv]{8}){7}\b")

_spec = importlib.util.spec_from_file_location("installer_console", REPO / "os/dev/installer-console.py")
console = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(console)


class Secrets:
    """Every secret this run handles, for redaction and the final scrub."""

    def __init__(self) -> None:
        self.values: list[str] = []

    def add(self, value: str) -> str:
        if value and value not in self.values:
            self.values.append(value)
        return value

    def redact(self, text: str) -> str:
        for value in self.values:
            text = text.replace(value, "[secret]")
        return RECOVERY_SHAPE.sub("[recovery-key-shaped]", text)

    def scrub(self, directory: Path) -> list[Path]:
        withheld = []
        for path in sorted(p for p in directory.rglob("*") if p.is_file()):
            data = path.read_bytes()
            text = data.decode("utf-8", "replace")
            if any(v and v in text for v in self.values) or RECOVERY_SHAPE.search(text) \
                    or "::tilo-recovery-key::" in text:
                path.write_bytes(b"")
                withheld.append(path)
        return withheld


SECRETS = Secrets()
LOG: Path | None = None


def say(line: str) -> None:
    line = SECRETS.redact(f"[{time.strftime('%H:%M:%S')}] {line}")
    print(line, flush=True)
    if LOG:
        with LOG.open("a", encoding="utf-8") as handle:
            handle.write(line + "\n")


class Failed(Exception):
    """A step that could not be carried out; its item is reported failed with this reason."""


class Cycle:
    """One named vm.sh cycle: an installer boot, then any number of target boots."""

    def __init__(self, name: str, medium: Path, storage: Path, out: Path) -> None:
        self.name = name
        self.out = out
        self.run = Path(os.environ.get("XDG_RUNTIME_DIR") or "/tmp") / f"tilo-vm-{name}"
        self.env = dict(os.environ, TILO_VM_CYCLE=name, TILO_INSTALLER_OUTPUT=str(storage),
                        TILO_INSTALLER_DISK=str(medium), TILO_RUN=str(self.run),
                        TILO_DISTROBOX="", TILO_TPM="1", TILO_ACCEL="kvm",
                        TILO_MEM=os.environ.get("TILO_MEM", "8192"),
                        TILO_SMP=os.environ.get("TILO_SMP", "4"), TILO_TARGET_SIZE="40G")
        self.shell_socket = self.run / "test-shell.sock"
        self.boots = 0

    def vm(self, *args: str, stdin: str | None = None, check: bool = True,
           timeout: float = 300) -> subprocess.CompletedProcess:
        result = subprocess.run(["bash", str(VM_SH), *args], env=self.env, input=stdin,
                                capture_output=True, text=True, timeout=timeout, cwd=REPO)
        if check and result.returncode != 0:
            raise Failed(f"vm.sh {args[0]} failed: {SECRETS.redact(result.stderr.strip()[-800:])}")
        return result

    @property
    def console(self) -> str:
        return str(self.run / "console.sock")

    def running(self) -> bool:
        return self.vm("status", check=False).stdout.strip().startswith("running")

    def wait_stopped(self, limit: float) -> bool:
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            if not self.running():
                return True
            time.sleep(2)
        return False

    def stop(self) -> None:
        self.vm("stop", check=False)

    def keys(self, *names: str) -> None:
        self.vm("keys", *names)

    def type(self, text: str) -> None:
        """Typed with QEMU key events; the text travels on stdin, never argv."""
        self.vm("type", "-", stdin=text + "\n", timeout=600)

    def hmp(self, command: str) -> str:
        return self.vm("cmd", command).stdout

    def shot(self, name: str) -> Path | None:
        result = self.vm("shot", name, check=False)
        path = Path(result.stdout.strip().splitlines()[-1]) if result.stdout.strip() else None
        return path if result.returncode == 0 and path and path.is_file() else None

    def screen(self, name: str = "observe") -> tuple[Path | None, str]:
        path = self.shot(name)
        if not path:
            return None, ""
        ocr = subprocess.run(["tesseract", str(path), "stdout"], capture_output=True, text=True)
        return path, ocr.stdout if ocr.returncode == 0 else ""

    def keep_screen(self, name: str, path: Path | None, text: str) -> None:
        """A screenshot and its text as check outputs. Never called for a screen showing a secret."""
        if path and path.is_file():
            shutil.copy(path, self.out / f"{name}.png")
            (self.out / f"{name}.txt").write_text(text, encoding="utf-8")

    def greeter_state(self, timeout: float = 3) -> dict | None:
        try:
            current = console.state(self.console, timeout)
        except (console.Unreachable, console.Refused):
            return None
        return current if current.get("app") == "greeter" else None

    def invoke(self, provider: str, action: str, args: dict | None = None,
               timeout: float = 60) -> object:
        reply = console.request(self.console, {"op": "actions_invoke", "provider": provider,
                                               "id": action, "args": args or {}}, timeout)
        return console.unwrap(reply, "answer")

    def hotplug_shell(self) -> None:
        """The test shell's port; the guest's unit (or the live installer's loop) opens it."""
        self.shell_socket.unlink(missing_ok=True)
        self.hmp(f"chardev-add socket,id=e2eshell,path={self.shell_socket},server=on,wait=off")
        reply = self.hmp(f"device_add virtserialport,chardev=e2eshell,name={SHELL_PORT},id=e2eport")
        if "rror" in reply:
            raise Failed(f"could not hot-plug the test shell port: {reply.strip()}")

    def keep_serial(self, label: str) -> None:
        serial = self.run / "serial.log"
        if serial.is_file():
            text = serial.read_bytes().replace(b"\0", b"").decode("utf-8", "replace")
            (self.out / f"serial-{label}.log").write_text(SECRETS.redact(text), encoding="utf-8")


class Shell:
    """A root shell in the guest over the hot-plugged virtio-serial port."""

    def __init__(self, path: Path, limit: float = 600) -> None:
        deadline = time.monotonic() + limit
        self.sock = None
        while time.monotonic() < deadline:
            try:
                sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                sock.connect(str(path))
                self.sock = sock
                break
            except OSError:
                time.sleep(1)
        if self.sock is None:
            raise Failed("the test shell port never appeared")
        self.buffer = b""
        while time.monotonic() < deadline:
            token = secrets.token_hex(6)
            try:
                self.sock.sendall(f"echo E2E-READY-{token}\n".encode())
                if self._read_until(re.compile(rf"E2E-READY-{token}".encode()), 3):
                    return
            except OSError as error:
                raise Failed(f"the test shell port closed: {error}") from error
        raise Failed("no root shell answered on the test shell port (is the target's test unit running?)")

    def _read_until(self, pattern: re.Pattern, limit: float) -> re.Match | None:
        deadline = time.monotonic() + limit
        while True:
            found = pattern.search(self.buffer)
            if found:
                return found
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            self.sock.settimeout(remaining)
            try:
                chunk = self.sock.recv(65536)
            except socket.timeout:
                return None
            if not chunk:
                raise Failed("the test shell closed its port")
            self.buffer += chunk

    def run(self, command: str, limit: float = 120, quiet: bool = False) -> tuple[int, str]:
        nonce = secrets.token_hex(8)
        self.buffer = b""
        # A subshell, so a command that exits ends only itself, not the shell on the port.
        script = f"( {command}\n) </dev/null 2>&1; printf '\\n%s %d\\n' E2E-DONE-{nonce} $?\n"
        self.sock.sendall(script.encode())
        found = self._read_until(re.compile(rf"\nE2E-DONE-{nonce} (\d+)\n".encode()), limit)
        if not found:
            raise Failed(f"guest command timed out: {SECRETS.redact(command)[:200]}")
        output = self.buffer[:found.start()].decode("utf-8", "replace")
        self.buffer = self.buffer[found.end():]
        code = int(found.group(1))
        if not quiet:
            say(f"guest$ {command[:160]} -> {code}")
        return code, output

    def close(self) -> None:
        if self.sock:
            self.sock.close()


def quote(value: str) -> str:
    return "'" + value.replace("'", "'\"'\"'") + "'"


# The test-only unit written into the installed target's /etc. Its port exists only when this
# check hot-plugs it; without the port the loop only waits.
SHELL_SCRIPT = f"""#!/usr/bin/bash
# signin-e2e check, this throwaway disk only: a root shell on {SHELL_PORT}.
echo 'tilo-e2e-shell: waiting for {SHELL_PORT}'
while :; do
  if [ -e /dev/virtio-ports/{SHELL_PORT} ]; then
    bash -c 'exec <>/dev/virtio-ports/{SHELL_PORT} >&0 2>&0; exec bash'
  fi
  sleep 1
done
"""
SHELL_UNIT = """[Unit]
Description=signin-e2e check: root shell on a hot-plugged virtio-serial port (this test disk only)
DefaultDependencies=no
After=local-fs.target

[Service]
ExecStart=/etc/tilo-e2e-shell
Restart=always
StandardOutput=journal+console

[Install]
WantedBy=multi-user.target
"""


class Run:
    def __init__(self, args: argparse.Namespace, variant: str) -> None:
        self.variant = variant
        self.out = args.out / variant
        self.out.mkdir(parents=True, exist_ok=True)
        self.private = Path(tempfile.mkdtemp(prefix="signin-e2e-", dir="/var/tmp"))
        self.cycle = Cycle(f"signin-{variant}-{os.getpid()}", args.medium, args.storage, self.out)
        self.user = "ada"
        self.full_name = "Ada Example"
        self.account = SECRETS.add("tilosignin7horse4battery")
        self.wrong = SECRETS.add("tilosignin7horse4batterz")
        self.disk = SECRETS.add("tilodisk9meadow2lantern5") if variant == "separate-disk-secret" else None
        self.recovery: str | None = None
        self.baseline: dict = {}
        self.items: dict[str, dict] = {}
        self.facts: dict = {"variant": variant, "cycle": self.cycle.name}
        self.shell: Shell | None = None

    # -- reporting ------------------------------------------------------------------------------

    def item(self, key: str, status: str, summary: str, **evidence: object) -> None:
        self.items[key] = {"status": status, "summary": summary, "evidence": evidence}
        say(f"ITEM {key}: {status.upper()} - {summary}")

    def save(self, name: str, content: str | dict | list) -> None:
        if not isinstance(content, str):
            content = json.dumps(content, indent=2, sort_keys=True) + "\n"
        (self.out / name).write_text(SECRETS.redact(content), encoding="utf-8")

    # -- the installer ------------------------------------------------------------------------

    def install(self) -> None:
        c = self.cycle
        c.vm("init-cycle")
        # The medium is unsigned: its installer admits the target only with the VM proof port.
        c.vm("boot-installer", "--2d", "--tpm", "--install-proof", timeout=600)
        say("live installer booting")
        try:
            console.poll(c.console, lambda s: s.get("app") == "installer", 900, 5)
        except TimeoutError as error:
            raise Failed("the live installer never answered on its console") from error
        last, repeats = "", 0
        while True:
            state = console.state(c.console, 30)
            step = state.get("step", "unknown")
            repeats = repeats + 1 if step == last else 0
            if repeats > 3:
                raise Failed(f"the installer stays on {step}: {state.get('status')}")
            last = step
            say(f"installer: {step}")
            if step in ("keyboard", "network", "media"):
                c.invoke("installer", "next@installer")
            elif step == "accounts":
                c.invoke("installer", "skip-offline@installer.accounts")
            elif step == "disk":
                c.invoke("installer", "set-method@installer.storage", {"method": "erase"})
                disks = [d for d in (state.get("storage") or {}).get("disks", [])
                         if not d.get("installer_medium") and not d.get("read_only")]
                if len(disks) != 1:
                    raise Failed(f"expected one target disk, the installer lists {disks}")
                c.invoke("installer", f"select@installer.disk:{disks[0]['device']}")
                c.invoke("installer", "next@installer")
            elif step == "account":
                self.facts["installer_secret_policy"] = (state.get("account") or {}).get("secret_policy")
                c.invoke("installer", "set-identity@installer.account",
                         {"username": self.user, "full_name": self.full_name})
                if self.disk:
                    c.invoke("installer", "set-disk-passphrase@installer.account",
                             {"passphrase": self.disk, "confirmation": self.disk})
                c.invoke("installer", "set-passphrase@installer.account",
                         {"passphrase": self.account, "confirmation": self.account})
                c.invoke("installer", "next@installer")
            elif step == "options":
                c.invoke("installer", "set-hostname@installer.options", {"hostname": "signin-e2e"})
                c.invoke("installer", "next@installer")
            elif step == "review":
                c.invoke("installer", "set-recovery-promise@installer.review", {"acknowledged": True})
                target = (console.state(c.console, 30).get("storage") or {}).get("confirmation_target")
                if not target:
                    raise Failed("the installer names no confirmation target")
                c.invoke("installer", "begin@installer.review", {"confirmation_phrase": target})
            elif step == "install":
                break
            else:
                raise Failed(f"the installer is on a step this check does not know: {step}")
        started = time.monotonic()
        seen: list = []

        def finished(state: dict) -> bool:
            install = state.get("install") or {}
            for stage in (install.get("stages") or [])[len(seen):]:
                seen.append(stage)
                say(f"install stage after {time.monotonic() - started:.0f}s: {SECRETS.redact(str(stage))}")
            return install.get("state") in ("done", "failed")

        try:
            final = console.poll(c.console, finished, 3600, 10)
        except TimeoutError as error:
            try:
                tail = (console.state(c.console, 30).get("install") or {}).get("log_tail", [])[-8:]
            except Exception as reason:  # the console itself may be the thing that stopped
                tail = [f"no installer state: {reason}"]
            raise Failed("the install did not finish within an hour; its last lines: "
                         + " | ".join(map(str, tail))) from error
        self.save("installer-final-state.json", final)
        if final["install"]["state"] != "done":
            raise Failed("the install failed: " + " | ".join(final["install"].get("log_tail", [])[-5:]))
        self.facts["install_seconds"] = round(time.monotonic() - started)
        if not (final.get("recovery_key") or {}).get("available"):
            raise Failed("the install finished without a recovery key")
        c.invoke("installer", "copy@installer.recovery")
        c.invoke("installer", "confirm-saved@installer.recovery", {"saved": True})
        self.prepare_target_from_live()
        try:
            c.invoke("installer", "power-off@installer.power")
        except (console.Unreachable, console.Refused):
            pass
        if not c.wait_stopped(180):
            raise Failed("the installer's power-off did not stop the VM")
        (self.out / "serial-installer-withheld.log").write_text(
            "withheld: the live installer's serial may carry the one-time recovery key\n")

    def live_shell(self) -> Shell:
        """The live installer's tty2 recovery tools: option 5 is a root shell. Its first command
        starts the loop that serves a shell on the hot-plugged port."""
        c = self.cycle
        c.hotplug_shell()
        c.keys("ctrl-alt-f2")
        time.sleep(4)
        c.type("5")
        c.keys("ret")
        time.sleep(3)
        c.type(f"setsid -f sh -c 'while :; do test -e /dev/virtio-ports/{SHELL_PORT} && bash -c "
               f"\"exec <>/dev/virtio-ports/{SHELL_PORT} >&0 2>&0; exec bash\"; sleep 1; done' "
               "</dev/null >/dev/null 2>&1")
        c.keys("ret")
        time.sleep(2)
        c.keys("ctrl-alt-f1")
        return Shell(c.shell_socket, 120)

    def prepare_target_from_live(self) -> None:
        sh = self.live_shell()
        try:
            code, dev = sh.run(f"lsblk -lnpo NAME,PARTTYPE /dev/vdb | awk 'tolower($2)==\"{TILO_ROOT_TYPE}\" {{print $1}}'")
            dev = dev.strip()
            if code or not dev.startswith("/dev/"):
                raise Failed(f"no Tilo root partition on the target: {dev}")
            self.facts["target_root_partition"] = dev
            self.recovery = self.read_recovery_key(sh)
            self.baseline = self.keyslot_view(sh, dev)
            self.save("keyslots-after-install.json", self.baseline)
            unlock = self.disk or self.account
            opened = self.slot_opened_by(sh, dev, unlock)
            self.baseline["disk_secret_slot"] = opened
            if self.recovery:
                self.baseline["recovery_slot"] = self.slot_opened_by(sh, dev, self.recovery)
            say(f"after install: keyslots {sorted(self.baseline['keyslots'])}, the disk secret opens "
                f"{opened}, the recovery key opens {self.baseline.get('recovery_slot')}")
            self.inject_test_shell(sh, dev, unlock)
        finally:
            sh.close()

    def read_recovery_key(self, sh: Shell) -> str | None:
        """The key the installer just copied to its clipboard, read back through wl-paste as the
        installer's user. Kept in memory only and redacted from everything this run writes."""
        script = r"""
for dir in /run/user/*; do
  for sock in "$dir"/wayland-[0-9]*; do
    case "$sock" in *.lock) continue ;; esac
    [ -S "$sock" ] || continue
    owner=$(stat -c %U "$dir")
    runuser -u "$owner" -- env XDG_RUNTIME_DIR="$dir" WAYLAND_DISPLAY="${sock##*/}" \
      timeout 10 wl-paste -n 2>/dev/null && exit 0
  done
done
exit 1
"""
        code, text = sh.run(script, quiet=True)
        found = RECOVERY_SHAPE.search(text)
        if code == 0 and found:
            say("read the recovery key from the installer's clipboard")
            return SECRETS.add(found.group(0))
        say("the installer's clipboard held no recovery key; reading it from the screen")
        path, text = self.cycle.screen("private-recovery")
        if path:
            ocr = subprocess.run(["tesseract", str(path), "stdout", "-c",
                                  "tessedit_char_whitelist=cbdefghijklnrtuv-"],
                                 capture_output=True, text=True)
            path.unlink(missing_ok=True)
            found = RECOVERY_SHAPE.search(ocr.stdout.replace(" ", ""))
            if found:
                return SECRETS.add(found.group(0))
        return None

    def keyslot_view(self, sh: Shell, dev: str) -> dict:
        """The LUKS2 header's keyslots and tokens, and a hash of each keyslot's key material."""
        code, text = sh.run(f"cryptsetup luksDump --dump-json-metadata {dev}", quiet=True)
        if code:
            raise Failed(f"cryptsetup luksDump failed: {text.strip()[-300:]}")
        meta = json.loads(text[text.index("{"):])
        areas = {}
        for slot, keyslot in meta.get("keyslots", {}).items():
            area = keyslot.get("area", {})
            _, digest = sh.run(f"dd if={dev} iflag=skip_bytes,count_bytes skip={area.get('offset')} "
                               f"count={area.get('size')} status=none | sha256sum | cut -d' ' -f1",
                               quiet=True)
            areas[slot] = digest.strip()
        return {"keyslots": meta.get("keyslots", {}), "tokens": meta.get("tokens", {}),
                "area_sha256": areas}

    def slot_opened_by(self, sh: Shell, dev: str, secret: str) -> int | None:
        code, text = sh.run(f"printf %s {quote(secret)} | cryptsetup open --test-passphrase "
                            f"--key-file=- -v {dev}", quiet=True)
        found = re.search(r"Key slot (\d+) unlocked", text)
        return int(found.group(1)) if code == 0 and found else None

    def inject_test_shell(self, sh: Shell, dev: str, unlock: str) -> None:
        script = f"""(
set -e
printf %s {quote(unlock)} | cryptsetup open --key-file=- {dev} e2e-target
mkdir -p /run/e2e-target
mount /dev/mapper/e2e-target /run/e2e-target
etc=$( {{ ls -d /run/e2e-target/state/deploy/*/etc /run/e2e-target/ostree/deploy/*/deploy/*/etc; find /run/e2e-target -maxdepth 6 -type d -path '*/deploy/*/etc'; }} 2>/dev/null | head -1)
test -d "$etc/systemd/system"
cat > "$etc/tilo-e2e-shell" <<'E2E_SCRIPT'
{SHELL_SCRIPT}E2E_SCRIPT
chmod 0755 "$etc/tilo-e2e-shell"
cat > "$etc/systemd/system/tilo-e2e-shell.service" <<'E2E_UNIT'
{SHELL_UNIT}E2E_UNIT
mkdir -p "$etc/systemd/system/multi-user.target.wants"
ln -sf /etc/systemd/system/tilo-e2e-shell.service "$etc/systemd/system/multi-user.target.wants/tilo-e2e-shell.service"
chcon system_u:object_r:bin_t:s0 "$etc/tilo-e2e-shell"
chcon system_u:object_r:systemd_unit_file_t:s0 "$etc/systemd/system/tilo-e2e-shell.service"
chcon -h system_u:object_r:systemd_unit_file_t:s0 "$etc/systemd/system/multi-user.target.wants/tilo-e2e-shell.service"
echo "test shell written into $etc"
sync
umount /run/e2e-target
cryptsetup close e2e-target
)"""
        code, text = sh.run(script, 300, quiet=True)
        say("live: " + text.strip().replace("\n", " | ")[-400:])
        if code:
            sh.run("umount /run/e2e-target; cryptsetup close e2e-target", quiet=True)
            raise Failed(f"could not write the test shell into the target: {text.strip()[-400:]}")

    # -- target boots ------------------------------------------------------------------------

    def boot(self, label: str) -> None:
        c = self.cycle
        c.boots += 1
        say(f"--- boot {c.boots}: {label}")
        c.vm("boot-target", "--2d", "--tpm", timeout=600)
        c.hotplug_shell()

    def classify(self, text: str) -> str:
        lower = text.lower()
        if "unlock tilo" in lower or "disk passphrase or recovery" in lower:
            return "disk-prompt"
        if "incorrect password" in lower or "incorrect pin" in lower or "tries left" in lower \
                or "too many failed" in lower:
            return "signin-note"
        if "field empty" in lower or "can't sign in" in lower or "who's signing in" in lower \
                or "cant sign in" in lower:
            return "boot-signin"
        return "other"

    def wait_screen(self, label: str, kinds: set[str], limit: float = 600,
                    greeter_ends: bool = True) -> tuple[str, Path | None, str, dict | None]:
        """Watch the framebuffer and the greeter's console until one of `kinds` shows. Returns
        (kind, screenshot, text, greeter state). A greeter answering ends the wait when
        `greeter_ends`, whatever the screen shows."""
        c = self.cycle
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            state = c.greeter_state(2)
            if state is not None:
                self.greeter_log(label, state)
                if greeter_ends or "greeter" in kinds:
                    return "greeter", None, "", state
            path, text = c.screen()
            kind = self.classify(text)
            if kind in kinds:
                return kind, path, text, None
            if not c.running():
                raise Failed(f"the VM stopped while waiting for {sorted(kinds)} ({label})")
            time.sleep(1.5)
        path, text = c.screen()
        c.keep_screen(f"{label}-timeout", path, text)
        return "timeout", path, text, None

    def greeter_log(self, label: str, state: dict) -> None:
        with (self.out / f"greeter-states-{label}.jsonl").open("a", encoding="utf-8") as handle:
            handle.write(SECRETS.redact(json.dumps({"t": round(time.time(), 1), **state})) + "\n")

    def connect(self, limit: float = 600) -> Shell:
        if self.shell:
            self.shell.close()
        self.shell = Shell(self.cycle.shell_socket, limit)
        return self.shell

    def guest(self, command: str, limit: float = 120, quiet: bool = False) -> tuple[int, str]:
        assert self.shell is not None
        return self.shell.run(command, limit, quiet)

    def power_off(self, label: str) -> None:
        c = self.cycle
        self.collect_journal(label)
        try:
            self.guest("systemctl --no-block poweroff", 30)
        except Failed:
            pass
        if self.shell:
            self.shell.close()
            self.shell = None
        if not c.wait_stopped(240):
            say("the guest did not power off; stopping the VM")
            c.stop()
        c.keep_serial(label)

    def collect_journal(self, label: str) -> None:
        if not self.shell:
            return
        try:
            _, text = self.guest("journalctl -b --no-pager -o short-monotonic | tail -n 6000", 120, True)
            self.save(f"journal-{label}.txt", text)
        except Failed as error:
            say(f"could not collect the journal of {label}: {error}")
        self.collect_signin_state(label)

    SIGNIN_STATE = {
        "units": "journalctl -b --no-pager -o short-monotonic -u greetd -u tilo-authd "
                 "-u tilo-authd.socket -u 'tilo-firstboot*' | tail -n 300",
        "avc": "ausearch -m avc,user_avc -ts boot 2>/dev/null || journalctl -b --no-pager -o cat "
               "| grep -E 'avc: +denied' | tail -n 100",
        "pam": "for f in greetd system-auth password-auth; do echo \"== /etc/pam.d/$f\"; "
               "cat /etc/pam.d/$f; done; echo '== pam_tilo'; ls -lZ /usr/lib64/security/pam_tilo.so",
        "authselect": "authselect current",
        "failed": "systemctl --failed --no-pager --plain --no-legend; "
                  "systemctl status --no-pager tilo-authd.socket tilo-authd.service greetd | tail -n 60",
        "sessions": "loginctl list-sessions --no-pager",
    }

    def collect_signin_state(self, label: str) -> None:
        """The sign-in stack as the guest sees it, saved on every boot: its units' journal, AVC
        denials, the PAM files, the authselect profile, failed units and sessions, and the
        greeter's console state."""
        for name, command in self.SIGNIN_STATE.items():
            try:
                _, text = self.guest(command, 60, True)
            except Failed as error:
                text = f"not collected: {error}"
            self.save(f"signin-{label}-{name}.txt", text)
        try:
            state = console.state(self.cycle.console, 10)
        except Exception as reason:  # no greeter or installer answers once a session runs
            state = {"unreachable": str(reason)}
        self.save(f"signin-{label}-console.json", state)

    def uid(self) -> int:
        code, text = self.guest(f"id -u {self.user}")
        if code:
            raise Failed(f"the account {self.user} does not exist: {text.strip()}")
        return int(text.strip())

    def user_session(self) -> dict | None:
        _, text = self.guest("loginctl list-sessions -o json --no-pager", quiet=True)
        try:
            sessions = json.loads(text[text.index("["):])
        except ValueError:
            return None
        for session in sessions:
            if session.get("user") == self.user and session.get("class", "user") == "user":
                return session
        return None

    def wait_session(self, limit: float = 240) -> dict | None:
        deadline = time.monotonic() + limit
        while time.monotonic() < deadline:
            session = self.user_session()
            if session:
                return session
            time.sleep(3)
        return None

    def watch_greeter_prompts(self, label: str, seconds: float) -> list[dict]:
        """Greeter states that asked the person for anything during `seconds`."""
        prompts = []
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            state = self.cycle.greeter_state(2)
            if state is not None:
                self.greeter_log(label, state)
                if state.get("phase") == "sign-in" or state.get("challenge"):
                    prompts.append(state)
            time.sleep(1)
        return prompts

    def lockout(self) -> dict:
        code, text = self.guest("tpm2_getcap properties-variable", 60, True)
        values = {}
        for name in ("TPM2_PT_LOCKOUT_COUNTER", "TPM2_PT_MAX_AUTH_FAIL", "TPM2_PT_LOCKOUT_INTERVAL",
                     "TPM2_PT_LOCKOUT_RECOVERY"):
            found = re.search(rf"{name}:\s*(0x[0-9a-fA-F]+|\d+)", text)
            if found:
                values[name] = int(found.group(1), 0)
        values["read_at"] = round(time.time(), 1)
        if code or "TPM2_PT_LOCKOUT_COUNTER" not in values:
            raise Failed(f"tpm2_getcap could not read the lockout counter: {text.strip()[-300:]}")
        return values

    def kernel_keys(self) -> list[str]:
        """The sign-in's kernel keys still present: the initrd's linked volume key
        (`tilo-auth:volume-key`) and the boot handoff (`tilo-signin`). The shell links uid 0's
        user keyring into its session keyring first, so keys only a possessor may view show."""
        _, text = self.guest("command -v keyctl >/dev/null && keyctl link @u @s >/dev/null 2>&1; "
                             "grep -E 'tilo-auth:volume-key|tilo-signin' /proc/keys || true", quiet=True)
        return [line.strip() for line in text.splitlines() if line.strip()]

    def root_device(self) -> str:
        _, text = self.guest("cryptsetup status root | sed -n 's/^ *device: *//p'")
        dev = text.strip()
        if not dev.startswith("/dev/"):
            raise Failed(f"the root mapping names no device: {text.strip()}")
        return dev

    # -- the items ---------------------------------------------------------------------------

    def unlock_at_disk_prompt(self, label: str, secret: str, shot_name: str) -> int:
        """Type `secret` at each disk prompt that shows, at most twice. Returns how many prompts
        were answered before the boot moved on."""
        typed = 0
        while True:
            kind, path, text, _ = self.wait_screen(label, {"disk-prompt", "boot-signin"}, 600)
            if kind == "disk-prompt" and typed < 2:
                if typed == 0:
                    self.cycle.keep_screen(shot_name, path, text)
                self.cycle.type(secret)
                self.cycle.keys("ret")
                typed += 1
                time.sleep(12)
                continue
            return typed

    def first_boot(self) -> None:
        """Item 1: one secret at install, no second prompt at first boot."""
        label = "first-boot"
        self.boot(label)
        unlock = self.disk or self.account
        typed = 0
        kind, path, text, _ = self.wait_screen(label, {"disk-prompt", "boot-signin"}, 600)
        if kind == "disk-prompt":
            self.cycle.keep_screen("first-boot-disk-prompt", path, text)
            self.cycle.type(unlock)
            self.cycle.keys("ret")
            typed = 1
        elif kind == "greeter":
            pass
        else:
            self.cycle.keep_screen("first-boot-unexpected", path, text)
        self.connect()
        session = self.wait_session(300)
        prompts = self.watch_greeter_prompts(label, 25)
        again = self.classify(self.cycle.screen()[1]) == "disk-prompt"
        _, pending = self.guest("test -e /etc/tilo/firstboot-autologin-pending && echo pending || echo consumed")
        _, greetd = self.guest("journalctl -b -u greetd --no-pager -o cat | tail -n 40", quiet=True)
        self.save("first-boot-greetd.txt", greetd)
        _, sessions = self.guest("loginctl list-sessions --no-pager; systemctl --failed --no-pager "
                                 "--plain --no-legend", quiet=True)
        self.save("first-boot-sessions.txt", sessions)
        path, text = self.cycle.screen()
        self.cycle.keep_screen("first-boot-after-unlock", path, text)
        evidence = {"disk_prompts_answered": typed, "user_session": session,
                    "greeter_prompts": prompts, "disk_prompt_again": again,
                    "firstboot_autologin_marker": pending.strip()}
        if typed == 1 and session and not prompts and not again:
            self.item("1", "passed", "one disk prompt at first boot, then the session started "
                      "without the greeter asking", **evidence)
        else:
            why = []
            if typed != 1:
                why.append(f"{typed} disk prompts answered (expected 1)")
            if not session:
                why.append("no session for the person started")
            if prompts:
                why.append(f"the greeter asked {len(prompts)} times")
            if again:
                why.append("the disk prompt came back")
            self.item("1", "failed", "; ".join(why), **evidence)
        self.power_off(label)

    def migrate(self) -> None:
        """Item 2: the person signs in once through the greeter."""
        label = "migration"
        self.boot(label)
        unlock = self.disk or self.account
        typed = self.unlock_at_disk_prompt(label, unlock, "migration-disk-prompt")
        try:
            state = console.poll(self.cycle.console, lambda s: s.get("app") == "greeter"
                                 and s.get("accepts_input"), 300, 2)
        except TimeoutError as error:
            path, text = self.cycle.screen()
            self.cycle.keep_screen("migration-no-greeter", path, text)
            try:
                last = console.state(self.cycle.console, 10)
            except Exception as reason:
                last = {"unreachable": str(reason)}
            self.save("migration-no-greeter-console.json", last)
            try:
                self.connect(120)
                _, sessions = self.guest("loginctl list-sessions --no-pager; systemctl --failed "
                                         "--no-pager --plain --no-legend; journalctl -b -u greetd "
                                         "--no-pager -o cat | tail -n 60", quiet=True)
                self.save("migration-no-greeter-guest.txt", sessions)
                self.collect_journal(label)
            except Failed as reason:
                say(f"no guest shell after the greeter wait: {reason}")
            raise Failed("the greeter never accepted input on the second boot") from error
        self.greeter_log(label, state)
        path, text = self.cycle.screen()
        self.cycle.keep_screen("greeter-before-sign-in", path, text)
        self.cycle.invoke("greeter", "sign-in@greeter", {"password": self.account})
        time.sleep(2)
        path, text = self.cycle.screen()
        self.cycle.keep_screen("greeter-signing-in", path, text)
        self.connect()
        session = self.wait_session(180)
        uid = self.uid()
        self.facts["uid"] = uid
        time.sleep(5)
        code, record_text = self.guest(f"cat /var/lib/tilo/auth/users/{uid}.json", quiet=True)
        record = {}
        if code == 0:
            try:
                record = json.loads(record_text[record_text.index("{"):])
            except ValueError:
                record = {}
        summary = {
            "kinds": [w.get("kind") for w in record.get("wrappers", [])],
            "system_keyslot": record.get("system_keyslot"),
            "unlocks_system": record.get("unlocks_system"),
            "attention": record.get("attention"),
            "secret": record.get("secret"),
        }
        self.save("record-after-migration.json", summary if record else {"missing": record_text})
        dev = self.root_device()
        after = self.keyslot_view(self.shell, dev)
        self.save("keyslots-after-migration.json", after)
        tokens = [t for t in after["tokens"].values() if t.get("type") == "tilo-signin"]
        slot = record.get("system_keyslot")
        token_slots = sorted({k for t in tokens for k in t.get("keyslots", [])})
        _, shadow = self.guest(f"getent shadow {self.user} | cut -d: -f2 | cut -c1-3", quiet=True)
        locked = shadow.strip()[:1] in ("!", "*")
        unchanged = {s: after["keyslots"].get(s) == self.baseline["keyslots"].get(s)
                     and after["area_sha256"].get(s) == self.baseline["area_sha256"].get(s)
                     for s in self.baseline["keyslots"]}
        reopen = {"disk_secret": self.slot_opened_by(self.shell, dev, unlock)}
        if self.recovery:
            reopen["recovery_key"] = self.slot_opened_by(self.shell, dev, self.recovery)
        keys = self.kernel_keys()
        volume_key_left = [k for k in keys if "tilo-auth:volume-key" in k]
        _, authd = self.guest("journalctl -b --no-pager -o cat -t tilo-authd -u tilo-authd | tail -n 60", quiet=True)
        self.save("tilo-authd-migration.txt", authd)
        evidence = {"disk_prompts_answered": typed, "session": session, "record": summary,
                    "tilo_signin_tokens": [{k: t.get(k) for k in ("keyslots", "uid", "name")}
                                           for t in tokens],
                    "shadow_locked": locked, "pre_existing_keyslots_unchanged": unchanged,
                    "pre_existing_secrets_open": reopen,
                    "baseline_slots": {"disk_secret": self.baseline.get("disk_secret_slot"),
                                       "recovery_key": self.baseline.get("recovery_slot")},
                    "tilo_authd": authd.strip().splitlines()[-8:],
                    "volume_key_left_in_keyring": volume_key_left}
        self.migration = evidence
        if self.variant == "separate-disk-secret":
            return
        checks = {
            "record with a tpm wrapper": "tpm" in summary["kinds"],
            "record names its system keyslot": isinstance(slot, int),
            "a tilo-signin token for that keyslot": isinstance(slot, int) and str(slot) in token_slots,
            "the keyslot is new": isinstance(slot, int) and str(slot) not in self.baseline["keyslots"],
            "shadow entry locked": locked,
            "no tilo-auth:volume-key left in the kernel keyring": not volume_key_left,
            "pre-existing keyslots unchanged": bool(unchanged) and all(unchanged.values()),
            "the disk secret still opens its slot":
                reopen["disk_secret"] is not None and reopen["disk_secret"] == self.baseline.get("disk_secret_slot"),
            "the recovery key still opens its slot":
                self.recovery is None or reopen.get("recovery_key") == self.baseline.get("recovery_slot"),
        }
        evidence["checks"] = checks
        if self.recovery is None:
            evidence["note"] = "the recovery key could not be read in the live installer; its slot was compared by header and key material only"
        failed = [name for name, ok in checks.items() if not ok]
        if failed:
            self.item("2", "failed", "; ".join(failed), **evidence)
        else:
            self.item("2", "passed", f"record with a tpm wrapper, keyslot {slot} with its tilo-signin "
                      "token, shadow locked, every pre-existing keyslot opens as before", **evidence)
        self.lockout_before = self.lockout()
        self.power_off(label)

    def boot_sign_in(self) -> None:
        """Item 3: the boot sign-in screen instead of the disk prompt, then the session without
        the greeter asking."""
        label = "boot-sign-in"
        self.boot(label)
        kind, path, text, _ = self.wait_screen(label, {"disk-prompt", "boot-signin"}, 600)
        evidence: dict = {"first_screen": kind}
        if kind != "boot-signin":
            self.cycle.keep_screen("boot-sign-in-missing", path, text)
            self.item("3", "failed", f"the boot showed {kind} where the boot sign-in screen belongs",
                      **evidence)
            self.recover_boot(label, kind)
            return
        self.cycle.keep_screen("boot-sign-in", path, text)
        self.cycle.type(self.account)
        self.cycle.keys("ret")
        time.sleep(1)
        path, text = self.cycle.screen()
        self.cycle.keep_screen("boot-sign-in-checking", path, text)
        prompts, disk_again = [], False
        deadline = time.monotonic() + 300
        session = None
        connected = False
        while time.monotonic() < deadline:
            state = self.cycle.greeter_state(2)
            if state is not None:
                self.greeter_log(label, state)
                if state.get("phase") == "sign-in" or state.get("challenge"):
                    prompts.append(state)
            else:
                path, text = self.cycle.screen()
                if self.classify(text) == "disk-prompt":
                    disk_again = True
                    self.cycle.keep_screen("boot-sign-in-disk-prompt-after", path, text)
                    break
            if not connected:
                try:
                    self.connect(5)
                    connected = True
                except Failed:
                    continue
            session = self.user_session()
            if session:
                break
            time.sleep(1)
        if connected:
            prompts += self.watch_greeter_prompts(label, 20)
        handoff_left = None
        if connected and session:
            time.sleep(5)
            handoff_left = [k for k in self.kernel_keys() if "tilo-signin" in k]
        evidence.update(user_session=session, greeter_prompts=prompts, disk_prompt_after=disk_again,
                        handoff_key_left=handoff_left)
        if connected:
            _, journal = self.guest("journalctl -b --no-pager -o short-monotonic | grep -Ei "
                                    "'tilo-authd|pam_tilo|greetd|tilo-boot-signin|tilo-root' | tail -n 80", quiet=True)
            self.save("boot-sign-in-journal.txt", journal)
            evidence["journal"] = [l for l in journal.splitlines()
                                   if "boot" in l.lower() and "tilo-authd" in l][-6:]
            evidence["greeter_states"] = sorted({s.get("phase") for s in self.read_greeter_log(label)})
        if session and not prompts and not disk_again and not handoff_left:
            self.item("3", "passed", "boot sign-in screen, one entry, the session started with no "
                      "greeter prompt (greeter phases seen: "
                      f"{evidence.get('greeter_states')})", **evidence)
        else:
            why = []
            if disk_again:
                why.append("the disk prompt followed the boot sign-in")
            if not session:
                why.append("no session started")
            if prompts:
                why.append(f"the greeter asked ({len(prompts)} prompt states)")
            if handoff_left:
                why.append(f"the tilo-signin handoff key is still in the kernel keyring: {handoff_left}")
            self.item("3", "failed", "; ".join(why), **evidence)
            if not connected:
                self.recover_boot(label, "stuck")
                return
        self.lockout_before = self.lockout()
        self.power_off(label)

    def read_greeter_log(self, label: str) -> list[dict]:
        path = self.out / f"greeter-states-{label}.jsonl"
        if not path.is_file():
            return []
        return [json.loads(l) for l in path.read_text().splitlines() if l.strip()]

    def recover_boot(self, label: str, kind: str) -> None:
        """Get past a boot that did not go as expected, to keep the later items observable."""
        if kind == "disk-prompt":
            self.cycle.type(self.disk or self.account)
            self.cycle.keys("ret")
            try:
                self.connect(300)
                self.power_off(label)
                return
            except Failed:
                pass
        self.cycle.stop()
        self.cycle.keep_serial(label)

    def wrong_entry(self) -> None:
        """Item 4: a wrong entry reports tries left, and the same wrong entry twice costs one
        TPM try. Item 5 follows in the same boot."""
        label = "wrong-entry"
        before = getattr(self, "lockout_before", None)
        self.boot(label)
        kind, path, text, _ = self.wait_screen(label, {"disk-prompt", "boot-signin"}, 600)
        if kind != "boot-signin":
            self.cycle.keep_screen("wrong-entry-no-boot-sign-in", path, text)
            self.item("4", "failed", f"no boot sign-in screen ({kind}), nothing to enter wrong",
                      lockout_before=before)
            self.recover_boot(label, kind)
            return
        notes = []
        for attempt in (1, 2):
            self.cycle.type(self.wrong)
            self.cycle.keys("ret")
            kind, path, text, _ = self.wait_screen(label, {"signin-note", "disk-prompt"}, 90,
                                                   greeter_ends=False)
            time.sleep(2)
            path, text = self.cycle.screen()
            self.cycle.keep_screen(f"boot-sign-in-wrong-{attempt}", path, text)
            found = re.search(r"(\d+)\s+tr(?:y|ies)\s+left", text, re.I)
            notes.append({"screen": self.classify(text), "tries_left": int(found.group(1)) if found else None,
                          "text": " ".join(text.split())[:200]})
        self.cycle.type(self.account)
        self.cycle.keys("ret")
        try:
            self.connect(400)
        except Failed as error:
            self.item("4", "failed", f"the correct entry after two wrong ones did not boot: {error}",
                      notes=notes, lockout_before=before)
            self.cycle.stop()
            return
        self.wait_session(180)
        after = self.lockout()
        evidence = {"notes": notes, "lockout_before": before, "lockout_after": after}
        reported = all(n["tries_left"] is not None for n in notes)
        if before is None:
            self.item("4", "failed", "no lockout counter was read before the wrong entries", **evidence)
        else:
            delta = after["TPM2_PT_LOCKOUT_COUNTER"] - before["TPM2_PT_LOCKOUT_COUNTER"]
            interval = after.get("TPM2_PT_LOCKOUT_INTERVAL") or 0
            elapsed = after["read_at"] - before["read_at"]
            evidence.update(counter_delta=delta, seconds_between_reads=round(elapsed),
                            recovery_interval_s=interval)
            problems = []
            if not reported:
                problems.append("the screen did not say how many tries are left "
                                f"(read: {[n['text'] for n in notes]})")
            if delta != 1:
                if delta == 0 and interval and elapsed >= interval:
                    problems.append(f"counter unchanged, but {round(elapsed)} s passed and the TPM "
                                    f"forgives one failure every {interval} s: inconclusive")
                else:
                    problems.append(f"the lockout counter moved by {delta}, not 1")
            if problems:
                self.item("4", "failed", "; ".join(problems), **evidence)
            else:
                self.item("4", "passed", f"tries left shown ({notes[0]['tries_left']}, then "
                          f"{notes[1]['tries_left']}); the same wrong entry twice cost one TPM try",
                          **evidence)
        self.pam_surfaces()
        self.power_off(label)

    def pam_surfaces(self) -> None:
        """Item 5: lock screen, sudo and polkit accept the secret through pam_tilo and reject a
        wrong one. With the shadow entry locked, pam_unix can accept nothing: an acceptance is
        pam_tilo's. Correct entries first, so a lockout from the wrong ones cannot hide them."""
        u = self.user
        before = self.lockout()
        _, stacks = self.guest("grep -l pam_tilo /etc/pam.d/system-auth /etc/pam.d/password-auth; "
                               "grep -E '^(auth|-auth)' /etc/pam.d/sudo /etc/pam.d/polkit-1 /etc/pam.d/tilo-unlock", quiet=True)
        self.save("pam-stacks.txt", stacks)
        _, shadow = self.guest(f"getent shadow {u} | cut -d: -f2 | cut -c1-3", quiet=True)
        checks = {}

        def attempt(name: str, command: str, secret: str) -> int:
            code, text = self.guest(command.replace("@SECRET@", quote(secret)), 120, True)
            say(f"{name}: exit {code}")
            checks.setdefault(name, []).append({"exit": code, "output": text.strip()[-200:]})
            return code

        lock = f"printf '%s\\n' @SECRET@ | runuser -u {u} -- /usr/libexec/tilo-unlock-helper {u}"
        sudo = f"printf '%s\\n' @SECRET@ | runuser -u {u} -- sudo -S -k -p '' /usr/bin/true"
        polkit_stack = f"printf '%s\\n' @SECRET@ | TILO_UNLOCK_PAM_SERVICE=polkit-1 /usr/libexec/tilo-unlock-helper {u}"
        pkexec = (f"{{ sleep 4; printf '%s\\n' @SECRET@; sleep 6; }} | runuser -u {u} -- "
                  "timeout 30 script -qefc 'pkexec /usr/bin/id -u' /dev/null | tr -d '\\r' | grep -qx 0")
        results = {}
        for name, command in (("lock-screen", lock), ("sudo", sudo), ("polkit", polkit_stack),
                              ("pkexec", pkexec)):
            results[name] = {"right": attempt(name, command, self.account)}
        for name, command in (("lock-screen", lock), ("sudo", sudo), ("polkit", polkit_stack),
                              ("pkexec", pkexec)):
            results[name]["wrong"] = attempt(name, command, self.wrong)
        after = self.lockout()
        evidence = {"results": results, "details": checks, "shadow_prefix_locked": shadow.strip()[:1] in ("!", "*"),
                    "lockout_before": before, "lockout_after": after}
        required = ("lock-screen", "sudo", "polkit")
        bad = []
        for n in required:
            if results[n]["right"] != 0:
                bad.append(f"{n} rejected the secret (exit {results[n]['right']})")
            if results[n]["wrong"] == 0:
                bad.append(f"{n} accepted a wrong entry")
        note = ("real pkexec through its text agent: "
                + ("accepted the secret and refused the wrong one"
                   if results["pkexec"]["right"] == 0 and results["pkexec"]["wrong"] != 0
                   else f"not conclusive (exit {results['pkexec']['right']} / {results['pkexec']['wrong']})"))
        evidence["pkexec_note"] = note
        if bad:
            self.item("5", "failed", "; ".join(bad), **evidence)
        else:
            self.item("5", "passed", "lock screen (tilo-unlock), sudo and polkit's PAM stack accept "
                      f"the secret and reject a wrong one; {note}", **evidence)

    def recovery_path(self) -> None:
        """Item 6: "Can't sign in", then the recovery key opens the volume."""
        label = "recovery"
        if not self.recovery:
            self.item("6", "failed", "the recovery key could not be read in the live installer "
                      "(clipboard and screen), so it could not be typed")
            return
        self.boot(label)
        kind, path, text, _ = self.wait_screen(label, {"disk-prompt", "boot-signin"}, 600)
        evidence: dict = {"first_screen": kind}
        if kind == "boot-signin":
            self.cycle.keep_screen("recovery-boot-sign-in", path, text)
            self.cycle.keys("ret")
            kind, path, text, _ = self.wait_screen(label, {"disk-prompt"}, 180)
            evidence["after_cant_sign_in"] = kind
        if kind != "disk-prompt":
            self.cycle.keep_screen("recovery-no-prompt", path, text)
            self.item("6", "failed", f"no prompt for the recovery key after \"Can't sign in\" ({kind})",
                      **evidence)
            self.cycle.stop()
            return
        self.cycle.keep_screen("recovery-prompt", path, text)
        self.cycle.type(self.recovery)
        self.cycle.keys("ret")
        try:
            state = console.poll(self.cycle.console, lambda s: s.get("app") == "greeter", 400, 3)
            self.greeter_log(label, state)
            path, text = self.cycle.screen()
            self.cycle.keep_screen("greeter-after-recovery", path, text)
            self.connect(300)
        except (TimeoutError, Failed) as error:
            path, text = self.cycle.screen()
            self.cycle.keep_screen("recovery-after-key", path, text)
            self.item("6", "failed", f"the recovery key did not open the volume: {error}", **evidence)
            self.cycle.stop()
            return
        _, mapped = self.guest("cryptsetup status root | head -n 1")
        evidence.update(greeter_phase=state.get("phase"), root=mapped.strip())
        self.item("6", "passed", "\"Can't sign in\" led to the volume prompt, the recovery key "
                  "opened it and the greeter asks as usual", **evidence)
        self.power_off(label)

    def separate_disk_secret(self) -> None:
        """Item 7: the disk passphrase differs from the login password."""
        evidence = dict(self.migration)
        slot = evidence["record"].get("system_keyslot")
        tokens = evidence["tilo_signin_tokens"]
        opened = evidence["pre_existing_secrets_open"]
        checks = {
            "record with a tpm wrapper": "tpm" in evidence["record"]["kinds"],
            "a new keyslot with its tilo-signin token": isinstance(slot, int)
            and str(slot) not in self.baseline["keyslots"]
            and any(str(slot) in t.get("keyslots", []) for t in tokens),
            "shadow entry locked": evidence["shadow_locked"],
            "pre-existing keyslots unchanged": all(evidence["pre_existing_keyslots_unchanged"].values()),
            "the disk secret still opens its slot": opened["disk_secret"] is not None
            and opened["disk_secret"] == self.baseline.get("disk_secret_slot"),
            "the recovery key still opens its slot": self.recovery is None
            or opened.get("recovery_key") == self.baseline.get("recovery_slot"),
            "no tilo-auth:volume-key left in the kernel keyring": not evidence["volume_key_left_in_keyring"],
        }
        evidence["checks"] = checks
        failed = [name for name, ok in checks.items() if not ok]
        migrated = checks["a new keyslot with its tilo-signin token"]
        if failed:
            self.item("7", "failed", "with a separate disk secret: " + "; ".join(failed)
                      + f" (record: {evidence['record']})", **evidence)
        else:
            self.item("7", "passed", f"with a separate disk secret, sign-in added keyslot {slot} and its "
                      "token from the linked volume key, which is gone afterwards; every pre-existing "
                      "keyslot opens as before", **evidence)
        self.lockout_before = self.lockout()
        self.power_off("migration")
        if migrated:
            self.boot_sign_in_variant()

    def boot_sign_in_variant(self) -> None:
        """With the separate disk secret migrated, the boot sign-in must take the login password."""
        saved = self.items.get("7")
        self.boot_sign_in()
        result = self.items.pop("3", None)
        if saved and result and result["status"] != "passed":
            saved["status"] = "failed"
            saved["summary"] += f"; but the next boot did not sign in with the password: {result['summary']}"
        if saved:
            saved["evidence"]["boot_sign_in"] = result
            self.items["7"] = saved

    # -- the variants -----------------------------------------------------------------------

    def execute(self) -> None:
        steps = [("install", self.install, ("1", "2", "3", "4", "5", "6"))]
        if self.variant == "one-secret":
            steps += [("first boot", self.first_boot, ("1",)),
                      ("migration", self.migrate, ("2", "3", "4", "5", "6")),
                      ("boot sign-in", self.boot_sign_in, ("3",)),
                      ("wrong entry", self.wrong_entry, ("4", "5")),
                      ("recovery key", self.recovery_path, ("6",))]
        else:
            steps = [("install", self.install, ("7",)),
                     ("first boot", self.first_boot_quiet, ("7",)),
                     ("migration", self.migrate, ("7",)),
                     ("separate disk secret", self.separate_disk_secret, ("7",))]
        for name, step, items in steps:
            try:
                step()
            except Failed as error:
                say(f"{name} could not be completed: {error}")
                for key in items:
                    self.items.setdefault(key, {"status": "failed",
                                                "summary": f"{name} could not be completed: {error}",
                                                "evidence": {}})
                try:
                    self.collect_journal(name.replace(" ", "-"))
                except Failed:
                    pass
                self.cycle.stop()
                self.cycle.keep_serial(name.replace(" ", "-"))
                if name in ("install", "migration", "first boot"):
                    break
        self.cycle.stop()

    def first_boot_quiet(self) -> None:
        """The separate-secret variant's first boot: item 1's path, reported under item 7."""
        saved = dict(self.items)
        self.first_boot()
        result = self.items.pop("1", None)
        self.items = saved
        self.facts["first_boot"] = result


def report(runs: list[Run], out: Path, withheld: list[Path]) -> int:
    items: dict[str, dict] = {}
    for run in runs:
        for key, value in run.items.items():
            items[key] = {**value, "variant": run.variant}
    names = {
        "1": "First boot after install: one secret, no second prompt",
        "2": "Migration at the greeter: record with tpm wrapper, keyslot + tilo-signin token, shadow locked, old keyslots open",
        "3": "Reboot: boot sign-in screen instead of the disk prompt, session without a greeter prompt",
        "4": "Wrong entry at boot reports tries left; the same wrong entry twice costs one TPM try",
        "5": "Lock screen, sudo and polkit accept the secret through pam_tilo and reject a wrong one",
        "6": "Recovery key opens the volume from \"Can't sign in\"",
        "7": "Separate disk passphrase: migration through the linked volume key, then boot sign-in",
    }
    for key in names:
        items.setdefault(key, {"status": "not-run", "summary": "an earlier step failed", "evidence": {}})
    document = {"items": {k: {"name": names[k], **items[k]} for k in sorted(items)},
                "facts": {r.variant: r.facts for r in runs},
                "screenshots": sorted(str(p.relative_to(out)) for p in out.rglob("*.png")),
                "withheld": [str(p.relative_to(out)) for p in withheld]}
    (out / "report.json").write_text(SECRETS.redact(json.dumps(document, indent=2, sort_keys=True,
                                                               default=str)) + "\n")
    lines = ["# One-password sign-in, end to end", ""]
    for key in sorted(items):
        entry = items[key]
        lines.append(f"- **{key}. {names[key]}**: {entry['status'].upper()} - {entry['summary']}")
    lines += ["", "Screenshots:", *[f"- {p}" for p in document["screenshots"]]]
    if withheld:
        lines += ["", "Withheld (held a secret, emptied):", *[f"- {p}" for p in document["withheld"]]]
    text = SECRETS.redact("\n".join(lines) + "\n")
    (out / "report.md").write_text(text)
    print(text)
    bad = [k for k, v in items.items() if v["status"] in ("failed", "not-run")]
    if bad:
        print_evidence(out, document)
    return 1 if bad or withheld else 0


JOURNAL_LINES = re.compile(r"greetd|greeter|tilo|pam_|systemd-logind|session|autologin|cryptsetup|"
                           r"plymouth|tpm|fail|error|denied", re.IGNORECASE)


def print_evidence(out: Path, document: dict) -> None:
    """A failed check publishes no outputs, so the log carries what the files hold: every item's
    evidence, each small text file, and each journal's lines about sign-in. All of it was
    redacted and scrubbed of secrets before this runs."""
    print("\n## Evidence\n")
    for key, item in sorted(document["items"].items()):
        print(f"### item {key} ({item.get('variant', '')}): {item['status']}")
        print(json.dumps(item.get("evidence", {}), indent=1, sort_keys=True, default=str)[:6000])
    for path in sorted(out.rglob("*")):
        if not path.is_file() or path.suffix not in (".txt", ".json", ".jsonl", ".log") \
                or path.name in ("report.json", "log.txt"):
            continue
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        if path.name.startswith("journal-") or path.name.startswith("serial-"):
            lines = [line for line in lines if JOURNAL_LINES.search(line)][-400:]
        else:
            lines = lines[-150:]
        print(f"\n### {path.relative_to(out)} ({len(lines)} lines shown)")
        print("\n".join(line[:400] for line in lines))


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--medium", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--storage", type=Path, required=True)
    parser.add_argument("--variant", action="append", choices=["one-secret", "separate-disk-secret"])
    args = parser.parse_args(argv)
    args.out.mkdir(parents=True, exist_ok=True)
    args.storage.mkdir(parents=True, exist_ok=True)
    global LOG
    LOG = args.out / "log.txt"
    runs = []
    for variant in args.variant or ["one-secret", "separate-disk-secret"]:
        run = Run(args, variant)
        runs.append(run)
        try:
            run.execute()
        finally:
            run.cycle.stop()
            shutil.rmtree(run.private, ignore_errors=True)
            shutil.rmtree(args.storage / "vm-cycles" / run.cycle.name, ignore_errors=True)
    withheld = SECRETS.scrub(args.out)
    return report(runs, args.out, withheld)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
