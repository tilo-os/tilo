#!/usr/bin/env python3
"""Branching a configured box, end to end, on real VMs.

    scripts/vm-e2e/branch.py --disk DEV.qcow2 --out DIR --taps BOX,A,B [--tilo-vm PATH] [--hold SECONDS]

tools/ci/vm-e2e-branch.sh runs it as the e2e-branch check, with the taps of the boxes' local
network.

A person sets one box up once, with their keys, and branches boxes off it that start
configured and then work on their own (`tilo-vm vm branch`, docs/architecture/workspace.md,
"A machine copied from another"). This proves it with this tree's dev disk and tilo-vm:

1. `box` boots and records its machine; a person signs in as the VM's person
   (`slot create --identity device`: the session's own onboarding creates the identity in the
   machine's identity tree and certifies the machine) and sets the box up: a Secret Service
   secret, a model provider's API key in their vault, a login in their passwords vault,
   `~/.gitconfig`, an SSH key, and a workspace of their own with a window on its desk.
2. `box` is stopped and branched into `a` and `b`, all three on one local network beside their
   own way out; they boot (b once box has done its part, where the host has no room for three).
3. Each branch has a device key of its own (the source's and the other branch's differ), a
   machine record of its own UUID, a valid certificate under the same person's root, the
   person's secret, API key, passwords vault, SSH key and git config readable as them, and a
   machine id and SSH host keys of its own; `box` keeps its own.
4. `box` and `a` hold the workspace: a window opened on either shows on the other, also
   after both reboot.
5. `vm rm box` leaves `a` and `b` bootable.

Everything goes through `tilo-vm` (scripts/vm-acceptance/acceptance/vm.py): `slot exec` as
the person, `vm ssh` as root, `control` for the desktop. The report (report.json,
report.md) and evidence go to --out. No secret value is printed: values are compared with
what was stored inside the guest.
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
import shlex
import subprocess
import sys
import time
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "vm-acceptance"))

from acceptance.vm import TiloVm, VmError, first_json, wait_until  # noqa: E402

SLOT = "me"
SOURCE, BRANCHES = "box", ("a", "b")
NAMES = (SOURCE, *BRANCHES)
# The boxes' local network (tools/ci/vm-e2e-branch.sh's bridge): each box's address on it and
# the MAC of its NIC there.
LAN = {name: (f"10.78.0.{n}/24", f"52:54:00:e2:b0:0{n}") for n, name in enumerate(NAMES, start=1)}
OUTPUT = "1280x800"
WORKSPACE = "Branch e2e"
# What the person stores: names only, never the values, appear in the report.
SECRET_ATTRS = {"service": "tilo-e2e-branch", "account": "person"}
GIT_NAME, GIT_EMAIL = "Ada Branch", "ada@branch.example"
PASSWORD_TITLE = "Branch e2e mail"
PASSWORDS = "so.tilo.passwords"
PROVIDER_LABEL = "Branch e2e"
PROVIDER_MODEL = "e2e-branch"
MODEL_PORT = 18431


class Failed(Exception):
    """A step observed something wrong."""


class Unscriptable(Exception):
    """What the step does is only reachable by a person's hands here: no action reaches it.
    Reported as `skip` with the reason, never as a pass."""


def expect(cond, message: str) -> None:
    if not cond:
        raise Failed(message)


# Each VM's run, for the helpers that keep evidence of it.
run_of: dict = {}


class Run:
    def __init__(self, binary: str, out: Path, taps: dict[str, str]):
        self.binary = binary
        self.out = out
        self.taps = taps
        self.steps: list[dict] = []
        self.vms: dict[str, TiloVm] = {}
        self.facts: dict = {}
        (out / "data").mkdir(parents=True, exist_ok=True)

    def vm(self, name: str) -> TiloVm:
        if name not in self.vms:
            self.vms[name] = TiloVm(self.binary, name, SLOT, self.out / f"tilo-vm-{name}.log")
            run_of[name] = self
        return self.vms[name]

    def write(self, name: str, text: str) -> None:
        (self.out / "data" / name).write_text(text)

    def step(self, sid: str, title: str, fn, *args):
        started = time.monotonic()
        entry = {"id": sid, "title": title, "status": "pass", "detail": "", "facts": {}}
        print(f"{sid} ... {title}", flush=True)
        try:
            value = fn(entry["facts"], *args)
        except Unscriptable as exc:
            entry["status"], entry["detail"] = "skip", str(exc)[:4000]
            value = None
        except (Failed, VmError) as exc:
            entry["status"], entry["detail"] = "fail", str(exc)[:4000]
            value = None
        except Exception as exc:  # noqa: BLE001 - a broken step is reported, the run goes on
            entry["status"] = "fail"
            entry["detail"] = f"{type(exc).__name__}: {exc}\n{traceback.format_exc()[-3000:]}"
            value = None
        entry["seconds"] = round(time.monotonic() - started, 1)
        self.steps.append(entry)
        print(f"{sid} {entry['status'].upper():4} {entry['seconds']:7.1f}s  {title}"
              + (f"\n      {entry['detail']}" if entry["detail"] else ""), flush=True)
        return value

    def passed(self, sid: str) -> bool:
        return any(s["id"] == sid and s["status"] == "pass" for s in self.steps)


# -- what the guest says --------------------------------------------------------

# As root: the machine's names and keys, by digest; the device key's public half where the
# image's Python can derive it. No private byte leaves the guest.
MACHINE_FACTS = r"""
cd /var/lib/tilo
echo "UUID=$(tr -d '-' < /sys/class/dmi/id/product_uuid | tr 'A-F' 'a-f')"
[ -f device-private/machine.bin ] && echo "RECORD=$(od -An -tx1 -v device-private/machine.bin | tr -d ' \n')"
[ -f device-private/keypair.bin ] && echo "KEY-DIGEST=$(sha256sum < device-private/keypair.bin | cut -c1-16)"
[ -f device-private/branch.bin ] && echo "JOURNAL=present"
echo "DEVICE-PUBLIC=$(python3 -I -c '
import sys
try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives import serialization
except ImportError:
    sys.exit(0)
seed = open("device-private/keypair.bin", "rb").read()
key = Ed25519PrivateKey.from_private_bytes(seed).public_key()
print(key.public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex())' 2>/dev/null)"
[ -f user-identity/pubkey.bin ] && echo "USER-PUBLIC=$(od -An -tx1 -v user-identity/pubkey.bin | tr -d ' \n')"
[ -f user-identity/privkey.bin ] && echo "ROOT-HERE=yes"
for c in device/certs/*.bin; do [ -f "$c" ] && echo "CERT=$(basename "$c" .bin)"; done
[ -f device/branch-label ] && echo "PENDING-LABEL=$(head -c 64 device/branch-label)"
echo "MACHINE-ID=$(cat /etc/machine-id)"
for k in /etc/ssh/ssh_host_*_key.pub; do [ -f "$k" ] && echo "HOST-KEY=$(ssh-keygen -lf "$k" | awk '{print $2}')"; done
echo "BRANCH-UNIT=$(systemctl show tilo-device-branch.service -p ActiveState -p Result --value | paste -sd, -)"
"""

# As the person, in their session: their identity and this device's certificate, what the
# Secret Service gives back for the secret and the API key (as digests), and their files.
PERSON_FACTS = r"""
echo "USER=$(id -un)"
echo "IDENTITY-BEGIN"; tilo-identity --json status 2>&1; echo "IDENTITY-END"
echo "STORE=$(tilo-secret-service status 2>&1 | head -1)"
# The store keeps an item for the program that stored it: read it back as secret-tool, which
# stored it, where the image has it.
if command -v secret-tool >/dev/null; then
  echo "SECRET-DIGEST=$(secret-tool lookup service __SERVICE__ account __ACCOUNT__ | sha256sum | cut -c1-16)"
else
  echo "SECRET-DIGEST=$(python3 -I "$XDG_RUNTIME_DIR/e2e-secret.py" get __SERVICE__ __ACCOUNT__ | sha256sum | cut -c1-16)"
fi
echo "GIT-NAME=$(git config --global user.name)"
echo "GIT-EMAIL=$(git config --global user.email)"
[ -f ~/.ssh/id_ed25519 ] && echo "SSH-KEY=$(ssh-keygen -lf ~/.ssh/id_ed25519 | awk '{print $2}')"
[ -f ~/.ssh/id_ed25519 ] && echo "SSH-KEY-READABLE=$(head -c 35 ~/.ssh/id_ed25519)"
t="${XDG_CONFIG_HOME:-$HOME/.config}/t/config.toml"
[ -f "$t" ] && echo "T-CONFIG=$(sha256sum < "$t" | cut -c1-16) $(stat -c %a "$t")"
"""

# The Secret Service as libsecret and secret-tool speak it (org.freedesktop.secrets, plain
# session): store a secret under attributes, or read one back by attributes. Run as the person.
SECRET_CLIENT = r'''
import sys
from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
NAME, PATH = "org.freedesktop.secrets", "/org/freedesktop/secrets"


def call(path, iface, method, args, reply):
    return bus.call_sync(NAME, path, iface, method, args, GLib.VariantType(reply),
                         Gio.DBusCallFlags.NONE, 30000, None).unpack()


_, session = call(PATH, "org.freedesktop.Secret.Service", "OpenSession",
                  GLib.Variant("(sv)", ("plain", GLib.Variant("s", ""))), "(vo)")


def unlock(paths):
    if paths:
        call(PATH, "org.freedesktop.Secret.Service", "Unlock", GLib.Variant("(ao)", (paths,)), "(aoo)")


def search(attrs):
    unlocked, locked = call(PATH, "org.freedesktop.Secret.Service", "SearchItems",
                            GLib.Variant("(a{ss})", (attrs,)), "(aoao)")
    unlock(locked)
    return unlocked + locked


def get(attrs):
    items = search(attrs)
    if not items:
        sys.exit("no such item")
    (secret,) = call(items[0], "org.freedesktop.Secret.Item", "GetSecret",
                     GLib.Variant("(o)", (session,)), "((oayays))")
    sys.stdout.buffer.write(bytes(secret[2]))


def store(attrs, label, value):
    collection = "/org/freedesktop/secrets/aliases/default"
    props = {"org.freedesktop.Secret.Item.Label": GLib.Variant("s", label),
             "org.freedesktop.Secret.Item.Attributes": GLib.Variant("a{ss}", attrs)}
    secret = (session, b"", value, "text/plain")
    call(collection, "org.freedesktop.Secret.Collection", "CreateItem",
         GLib.Variant("(a{sv}(oayays)b)", (props, secret, True)), "(oo)")


cmd = sys.argv[1]
if cmd == "store":
    store({"service": sys.argv[2], "account": sys.argv[3]}, "tilo e2e branch", sys.stdin.buffer.read())
elif cmd == "get":
    get({"service": sys.argv[2], "account": sys.argv[3]})
'''


# As root: a loopback OpenAI-compatible endpoint that lists one model to a caller presenting
# the API key whose digest is in /run/e2e-model.digest, and refuses anyone else. It logs
# whether each request's key was the one stored, never the key.
MODEL_SERVER = r'''
import hashlib, http.server, json, sys
want = open("/run/e2e-model.digest").read().strip()


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        auth = self.headers.get("authorization") or ""
        key = auth[7:] if auth.lower().startswith("bearer ") else auth
        ok = bool(key) and hashlib.sha256(key.encode()).hexdigest() == want
        with open("/run/e2e-model.log", "a") as log:
            log.write(f"{self.path} key={'stored' if ok else ('other' if key else 'none')}\n")
        body = json.dumps({"object": "list", "data": [{"id": "__MODEL__", "object": "model"}]} if ok
                          else {"error": {"message": "invalid api key"}}).encode()
        self.send_response(200 if ok else 401)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
'''


def fields(text: str) -> dict:
    out: dict = {}
    for line in text.splitlines():
        key, sep, value = line.partition("=")
        if sep and key.replace("-", "").isalpha() and key.isupper():
            if key in out:
                out[key] = (out[key] if isinstance(out[key], list) else [out[key]]) + [value.strip()]
            else:
                out[key] = value.strip()
    return out


def as_list(value) -> list:
    return value if isinstance(value, list) else ([value] if value else [])


def machine_facts(run: Run, name: str, label: str) -> dict:
    text = run.vm(name).root_exec(MACHINE_FACTS, timeout=120)
    run.write(f"{label}-machine.txt", text)
    f = fields(text)
    f["HOST-KEY"] = sorted(as_list(f.get("HOST-KEY")))
    f["CERT"] = sorted(as_list(f.get("CERT")))
    return f


def install_secret_client(vm: TiloVm) -> None:
    vm.exec('cat > "$XDG_RUNTIME_DIR/e2e-secret.py" <<\'EOF\'\n' + SECRET_CLIENT + "\nEOF\n")


def person_facts(run: Run, name: str, label: str) -> dict:
    vm = run.vm(name)
    install_secret_client(vm)
    script = (PERSON_FACTS.replace("__SERVICE__", SECRET_ATTRS["service"])
              .replace("__ACCOUNT__", SECRET_ATTRS["account"]))
    text = vm.exec(script, timeout=120, check=False)
    run.write(f"{label}-person.txt", text)
    f = fields(text)
    raw = text.split("IDENTITY-BEGIN", 1)[-1].split("IDENTITY-END", 1)[0].strip()
    try:
        f["identity"] = first_json(raw)
    except VmError:
        f["identity"] = {"unparsed": raw[-600:]}
    return f


# The digest of nothing: what a failed read prints.
EMPTY_DIGEST = "e3b0c44298fc1c14"


# -- the steps ---------------------------------------------------------------------


def mem_available_mb() -> int:
    for line in Path("/proc/meminfo").read_text().splitlines():
        if line.startswith("MemAvailable:"):
            return int(line.split()[1]) // 1024
    return 0


# What one guest gets, and what the host keeps beside them.
GUEST_MB, HOST_MB = 3072, 1536


def boot_box(facts: dict, run: Run, disk: Path, memory: int) -> None:
    box = run.vm(SOURCE)
    facts["memory_mb"] = memory
    box.vm_cmd("create", SOURCE, "--image", str(disk), "--heads", "1", "--resolution", OUTPUT,
               "--memory-mb", str(memory), "--cpus", "2", timeout=900)
    box.vm_cmd("start", SOURCE, timeout=900)
    m = machine_facts(run, SOURCE, "S01-box")
    facts["machine"] = {k: m.get(k) for k in ("UUID", "RECORD", "BRANCH-UNIT")}
    expect(m.get("UUID"), "box reports no product UUID")
    expect(m.get("RECORD") == m.get("UUID"),
           f"box did not record its machine at boot: machine.bin {m.get('RECORD')!r}, "
           f"product_uuid {m.get('UUID')!r} (tilo-device-branch.service: {m.get('BRANCH-UNIT')})")


def desk_up(vm: TiloVm) -> bool:
    """Whether the session shows its desk, unlocked, with no setup over it: what a person who
    turns the box on finds, read as e2e-remote reads it."""
    keep_awake(vm)
    try:
        dump = vm.state()
        names = [str(t.get("name")) for t in vm.targets()]
    except (VmError, ValueError):
        return False
    state = dump.get("state") or dump
    chrome = state.get("chrome") or {}
    return ("workspace_tree" in state and not chrome.get("session_locked")
            and any(n.startswith("DesktopChrome") for n in names)
            and not any(n.startswith("OnboardingView") for n in names) and "Setup" not in names)


def wait_session(vm: TiloVm, timeout: float = 300) -> bool:
    """Until the person's session answers and lists its apps; a slot whose session did not
    come up with the box is started as tilo-vm starts it. Whether it had to be."""
    if wait_until(lambda: desk_up(vm), timeout, interval=1.0):
        return False
    vm.run("slot", "restart", SLOT, timeout=300, slot=False)
    if not wait_until(lambda: desk_up(vm), timeout, interval=1.0):
        raise Failed(f"{vm.vm}'s session never showed its desk within {timeout:.0f}s")
    return True


IDENTITY_DIAGNOSIS = r"""
u=$(id -u tds-me)
echo "== account"; id tds-me; getent group tilo-identity
echo "== suid_dumpable $(cat /proc/sys/fs/suid_dumpable)"; getcap /usr/bin/tilo-session 2>&1
for p in $(pgrep -u "$u" -f /usr/bin/tilo-session); do
  echo "== process $p: $(tr '\0' ' ' </proc/$p/cmdline | cut -c1-120)"
  grep -E '^(Uid|Gid|Groups|CapPrm|CapEff|CapAmb|NoNewPrivs|Seccomp):' /proc/$p/status
done
echo "== identity files"
ls -la --time-style=full-iso ~tds-me/.config/tilo/identity /var/lib/tilo/device   /var/lib/tilo/device/certs /var/lib/tilo/device-private /var/lib/tilo/user-identity 2>&1
echo "== the session's own check, as the person"
runuser -u tds-me -- env XDG_RUNTIME_DIR=/run/user/$u DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$u/bus   /usr/bin/tilo-session --provision-identity 2>&1 | tail -n 20
echo "== the session's stderr this boot"
journalctl -m --since "$(uptime -s)" --no-pager -o short-iso _UID=$u 2>&1   | grep -a 'tilo-session-me\|secret' | grep -av 'FRAME-LIFECYCLE\|paint-stats\|HEAP-MILESTONE' | head -n 60
"""


def identity_diagnosis(vm: TiloVm) -> str:
    """What the person's identity check answers now and what it reads, from root: the
    session's process (its groups and capabilities), the identity files, the check run as
    the person, and what the session said this boot."""
    try:
        return vm.root_exec(IDENTITY_DIAGNOSIS, timeout=180, check=False)
    except VmError as exc:
        return f"diagnosis failed: {exc}"


def what_shows(vm: TiloVm) -> str:
    """What the session answers when it lists no apps: its chrome's flags, the targets it
    draws, and what listing its actions says."""
    parts = []
    try:
        dump = vm.state()
        state = dump.get("state") or dump
        chrome = state.get("chrome") or {}
        parts.append("chrome " + json.dumps({k: v for k, v in chrome.items()
                                             if isinstance(v, (bool, int, str))})[:600])
    except (VmError, ValueError) as exc:
        parts.append(f"state: {str(exc)[:400]}")
    try:
        parts.append("targets " + json.dumps([t.get("name") for t in vm.targets()][:40])[:800])
    except (VmError, ValueError) as exc:
        parts.append(f"targets: {str(exc)[:400]}")
    try:
        parts.append("actions " + vm.run("control", "actions", "list", "Terminal", "--json",
                                         "--limit", "20", timeout=60)[:600])
    except VmError as exc:
        parts.append(f"actions: {str(exc)[:800]}")
    return "; ".join(parts)


def desk_at_boot(run: Run, name: str, label: str) -> list[str]:
    """Whether the person's session came up at its desk on its own this boot: what stood in
    its way (setup or identity recovery, or a session that listed no apps), with what the
    session's own identity check answers now kept as evidence. Empty when the desk is up."""
    vm = run.vm(name)
    up = wait_until(lambda: desk_up(vm) or bool(setup_shown(vm)), 300, interval=1.0)
    shown = setup_shown(vm) if up else [f"the desk did not come up within 300s: {what_shows(vm)}"]
    if shown:
        run.write(f"{label}-{name}-verify-session.txt",
                  f"== what the session shows\n{'; '.join(shown)}\n" + identity_diagnosis(vm))
        run.write(f"{label}-{name}-session.txt", session_excerpt(vm))
        try:
            wait_session(vm)
        except Failed as exc:
            shown.append(f"and after a slot restart: {exc}: {what_shows(vm)}")
    return shown


# -- the boxes' local network --------------------------------------------------------


def virsh(*args: str, check: bool = True) -> str:
    proc = subprocess.run(["virsh", "--connect", "qemu:///session", *args],
                          capture_output=True, text=True, timeout=120)
    if check and proc.returncode != 0:
        raise Failed(f"virsh {' '.join(args)}: {proc.stderr.strip() or proc.stdout.strip()}")
    return proc.stdout


def plug_lan(run: Run, name: str) -> None:
    """Put the stopped `name` on the boxes' network: its tap, as a second NIC."""
    ip, mac = LAN[name]
    nic = run.out / "data" / f"lan-{name}.xml"
    nic.write_text(f"<interface type='ethernet'><mac address='{mac}'/>"
                   f"<target dev='{run.taps[name]}' managed='no'/><model type='virtio'/></interface>\n")
    virsh("attach-device", name, str(nic), "--config")


def address_lan(run: Run, name: str) -> str:
    """Give `name` its address on the boxes' network, as a person sets one by hand."""
    ip, mac = LAN[name]
    return run.vm(name).root_exec(f"""
set -e
dev=$(ip -o link | awk -v mac={mac} 'index($0, mac) {{ sub(":$", "", $2); print $2; exit }}')
[ -n "$dev" ] || {{ echo "no interface with {mac}"; ip -o link; exit 1; }}
if command -v nmcli >/dev/null 2>&1 && nmcli -t general status >/dev/null 2>&1; then
  nmcli con delete tilo-e2e >/dev/null 2>&1 || true
  nmcli con add type ethernet ifname "$dev" con-name tilo-e2e ipv4.method manual \
    ipv4.addresses {ip} ipv6.method link-local >/dev/null
  nmcli con up tilo-e2e >/dev/null
else
  ip link set "$dev" up
  ip addr replace {ip} dev "$dev"
fi
ip -o addr show dev "$dev"
""", timeout=120)


BOOT_JOURNAL = ("journalctl -b --no-pager -o short-monotonic -u tilo-device-branch "
                "-u tilo-device-key-init -u tilo-vmd -u tilo-vm-restore 2>&1 | tail -n 200; "
                "journalctl -b -k --no-pager 2>&1 | grep -i 'type=1326\\|seccomp' | tail -n 5; "
                "systemctl show tilo-device-branch -p ActiveState -p Result -p ExecMainStatus; "
                "ls -la /var/lib/tilo /var/lib/tilo/device-private /var/lib/tilo/device "
                "/var/lib/tilo/user-identity 2>&1")


def keep_boot(run: Run, name: str, label: str) -> str:
    """What the box's boot said of its device identity, for the evidence."""
    try:
        text = run.vm(name).root_exec(BOOT_JOURNAL, timeout=120, check=False)
    except VmError as exc:
        text = f"could not read the boot journal: {exc}"
    run.write(f"{label}-{name}-boot.txt", text)
    return text


def keep_session(run: Run, name: str, label: str) -> None:
    """What the person's session said and showed, for a step that could not reach it."""
    vm = run.vm(name)
    parts = []
    for args in (("logs", SLOT, "-n", "4000"),
                 ("control", "raw", '{"op":"list_targets"}'),
                 ("control", "actions", "list", "--json", "--limit", "300")):
        try:
            parts.append(f"$ tilo-vm {' '.join(args)}\n" + vm.run(*args, timeout=120, check=False,
                                                                 slot=args[0] != "logs"))
        except VmError as exc:
            parts.append(f"$ tilo-vm {' '.join(args)}: {exc}")
    try:
        targets = first_json(vm.run("control", "raw", '{"op":"list_targets"}', timeout=60, check=False))
        names = [t.get("name") for t in (targets.get("answer") or targets).get("targets") or []]
        parts.append("target names: " + json.dumps([n for n in names if n and len(n) < 80]))
    except (VmError, AttributeError) as exc:
        parts.append(f"target names: {exc}")
    noise = ("FRAME-LIFECYCLE", "paint-stats:", "HEAP-MILESTONE", "DEVICE-MEMORY", "frames=")
    run.write(f"{label}-{name}-session.txt",
              "\n".join(l for l in "\n".join(parts).splitlines() if not any(n in l for n in noise)))
    keep_boot(run, name, label)


def keep_actions(run: Run, name: str, label: str) -> None:
    """What the desktop lists, by provider and id, for steps that address its actions."""
    try:
        listed = listed_actions(run.vm(name))
        # The desk's own chrome last: what an app offers is what a failing step looks for, and the
        # evidence keeps the file's end.
        listed.sort(key=lambda r: r.get("provider") not in ("so.tilo.session", "applications"))
        rows = [f"{r.get('provider')}  {r.get('application') or ''}  "
                f"{str(r.get('id')).replace(chr(31), ' | ')[:160]}  {r.get('operation')}  "
                f"{r.get('title')}" for r in listed]
        run.write(f"{label}-{name}-actions.txt", "\n".join(rows))
    except VmError as exc:
        run.write(f"{label}-{name}-actions.txt", f"could not list: {exc}")


def sign_in(facts: dict, run: Run) -> None:
    box = run.vm(SOURCE)
    try:
        out = box.run("--json", "slot", "create", SLOT, "--headful", "--outputs", f"{OUTPUT}@1",
                      "--identity", "device", timeout=900, slot=False)
    except VmError:
        keep_session(run, SOURCE, "S02")
        raise
    run.write("S02-slot-create.json", out)
    wait_session(box)
    p = person_facts(run, SOURCE, "S02-box")
    ident = p.get("identity") or {}
    facts["identity"] = ident
    expect((ident.get("identity") or {}).get("id"), f"the person has no identity: {ident}")
    expect(ident.get("this_device"), f"box holds no valid certificate for the person: {ident}")
    # Where the person's root is decides how a branch is certified: at boot by the root in the
    # machine's identity tree, else at the person's next sign-in by the root in their record.
    m = machine_facts(run, SOURCE, "S02-box")
    facts["root_in_machine_tree"] = m.get("ROOT-HERE") == "yes"
    keep_actions(run, SOURCE, "S02")
    # The same diagnosis as at a boot that misses its desk, of a session at its desk.
    run.write("S02-box-diagnosis-session.txt", identity_diagnosis(run.vm(SOURCE)))


def configure(facts: dict, run: Run) -> None:
    """A secret in their Secret Service (as `secret-tool store` puts one), git and SSH."""
    box = run.vm(SOURCE)
    install_secret_client(box)
    have_tool = box.exec("command -v secret-tool || true").strip()
    facts["secret_tool"] = bool(have_tool)
    value = "e2e-branch-secret-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \\n')"
    if have_tool:
        stored = box.exec(f'printf %s "{value}" | secret-tool store --label "tilo e2e branch" '
                          f'service {SECRET_ATTRS["service"]} account {SECRET_ATTRS["account"]} 2>&1; '
                          'echo "exit=$?"', check=False)
    else:
        stored = box.exec(f'printf %s "{value}" | python3 -I "$XDG_RUNTIME_DIR/e2e-secret.py" store '
                          f'{SECRET_ATTRS["service"]} {SECRET_ATTRS["account"]} 2>&1; echo "exit=$?"',
                          check=False)
    facts["store"] = stored.strip()[-600:]
    read = box.exec(f'secret-tool lookup service {SECRET_ATTRS["service"]} account '
                    f'{SECRET_ATTRS["account"]} 2>&1 >/dev/null | tail -n 5; echo "exit=$?"',
                    check=False)
    facts["read_back"] = read.strip()[-600:]
    box.exec(f"git config --global user.name {shlex.quote(GIT_NAME)} && "
             f"git config --global user.email {shlex.quote(GIT_EMAIL)} && "
             "mkdir -p -m 700 ~/.ssh && ssh-keygen -q -t ed25519 -N '' -C ada@box -f ~/.ssh/id_ed25519")
    # Their t client's server and token, where t reads them (`dirs::config_dir()/t`): a token
    # made for this run, never a real one.
    token = "e2e-branch-" + os.urandom(16).hex()
    box.exec('d="${XDG_CONFIG_HOME:-$HOME/.config}/t" && mkdir -p -m 700 "$d" && umask 077 && '
             f'printf \'server = "https://t.tilo.so"\\ntoken = "{token}"\\n\' > "$d/config.toml"')
    p = person_facts(run, SOURCE, "S03-box")
    facts["person"] = {k: p.get(k) for k in ("SECRET-DIGEST", "GIT-NAME", "SSH-KEY", "T-CONFIG")}
    expect(p.get("T-CONFIG", "").endswith(" 600"), f"the t config is not the person's alone: {p.get('T-CONFIG')}")
    expect(p.get("SECRET-DIGEST") not in (None, EMPTY_DIGEST),
           f"the stored secret does not read back: store said {facts['store']!r}; "
           f"reading said {facts['read_back']!r}")
    expect(p.get("GIT-NAME") == GIT_NAME, "git config did not take")
    expect(p.get("SSH-KEY"), "no SSH key")


def configure_provider(facts: dict, run: Run) -> None:
    """A model provider's API key, saved through Settings into their vault."""
    box = run.vm(SOURCE)
    key = "sk-e2e-" + os.urandom(18).hex()
    digest = hashlib.sha256(key.encode()).hexdigest()
    serve_model(box, digest)
    add_provider(box, key)
    facts["provider"] = provider_answers(box)
    facts["api_key_digest"] = digest


def configure_passwords(facts: dict, run: Run) -> None:
    """A login in their passwords vault, saved through Passwords' own save-login action."""
    box = run.vm(SOURCE)
    open_passwords(box)
    before = password_items(box)
    password = "e2e-" + os.urandom(12).hex()
    app_action(box, "", "save-login", {"name": PASSWORD_TITLE, "origin": "https://branch.example",
                                       "username": "ada@branch.example",
                                       "password": password}, PASSWORDS)
    new = wait_until(lambda: password_items(box) - before, 30, interval=1.0)
    expect(new, "Passwords lists no new item after save-login")
    facts["password_items"] = sorted(new)
    expect(password not in json.dumps(listed_actions(box)),
           "the saved password is in the desktop's action listing")
    leaks = secret_leaks(box, password)
    expect(not leaks, f"the saved password is in plain text in {leaks}")


# Where a secret a person saved must never be in plain text: the system journal (every unit's
# and the session's own log) and the files under /var/log and the person's home, where the
# vault keeps it only sealed.
SECRET_PLACES = """p=$(printf %s {pattern} | base64 -d)
journalctl -m --no-pager -o cat 2>/dev/null | grep -qaF -- "$p" && echo journal
grep -rlaF --exclude-dir=proc -- "$p" /var/log /home 2>/dev/null | head -n 20
true"""


def secret_leaks(vm: TiloVm, secret: str) -> list[str]:
    """The places on `vm` that hold `secret` in plain text. The secret travels base64-encoded,
    so no command line or log of this harness carries it either."""
    pattern = shlex.quote(base64.b64encode(secret.encode()).decode())
    text = vm.root_exec(SECRET_PLACES.format(pattern=pattern), timeout=300)
    return [line for line in text.splitlines() if line.strip()]


def open_passwords(vm: TiloVm) -> None:
    if not of_app(app_rows(vm, "", "save-login"), PASSWORDS):
        open_app(vm, PASSWORDS)
    if not wait_until(lambda: of_app(app_rows(vm, "", "save-login"), PASSWORDS),
                      60, interval=1.0):
        keep_actions(run_of[vm.vm], vm.vm, "S03d")
        raise Failed(f"Passwords is open on {vm.vm}, but the desktop lists none of its "
                           "actions, so no login can be saved without a person's hands")


def password_items(vm: TiloVm) -> set[str]:
    """The vault items Passwords publishes rows for, by their resource ids."""
    return {part for row in listed_actions(vm) if row.get("provider") == "app-actions"
            for part in row["id"].split("\x1f") if part.startswith("passwords:item:")}


def configure_workspace(facts: dict, run: Run) -> None:
    """A workspace of their own, hosted on this device, with a window on its desk."""
    box = run.vm(SOURCE)
    ws = make_workspace(box)
    facts["workspace"] = ws
    facts["window_on_box"] = open_window(box, ws)


def keep_awake(vm: TiloVm) -> None:
    """A key on the slot's seat, as a person at it: an idle desk locks, and a locked one offers
    none of its actions."""
    try:
        vm.key("shift")
    except VmError:
        pass


def act(vm: TiloVm, provider: str, name: str, args: dict | None = None, wait: bool = False) -> dict:
    """Invoke the desktop action `name` of `provider`, addressed as `tilo-control actions list`
    prints it (`resource␟action`): the listed row whose id is `name` or ends in it."""
    keep_awake(vm)
    rows = [r for r in listed_actions(vm) if r.get("provider") == provider
            and (r["id"] == name or r["id"].endswith("\x1f" + name))]
    pages = "every"
    expect(rows, f"{vm.vm} lists no {provider} action {name!r} in {pages} page(s)")
    argv = ["actions", "invoke", provider, rows[0]["id"]]
    if wait:
        argv.append("--wait=20000")
    if args is not None:
        argv += ["--args-json", json.dumps(args)]
    return vm.control_json(*argv, timeout=90)


# -- a model provider's API key, through Settings ------------------------------------


def serve_model(vm: TiloVm, key_digest: str) -> None:
    """The loopback endpoint the provider points at, as root, for this boot."""
    vm.root_exec(f"printf %s {key_digest} > /run/e2e-model.digest && rm -f /run/e2e-model.log && "
                 "cat > /run/e2e-model.py <<'EOF'\n" + MODEL_SERVER.replace("__MODEL__", PROVIDER_MODEL)
                 + "\nEOF\nsystemctl stop e2e-model.service 2>/dev/null; systemctl reset-failed e2e-model.service "
                 f"2>/dev/null; systemd-run --unit e2e-model --quiet python3 -I /run/e2e-model.py {MODEL_PORT}")


def listed_actions(vm: TiloVm, *query: str) -> list[dict]:
    """Every row the desktop lists for `query`, page after page: a page holds at most 256 rows
    (`MAX_CATALOGUE_PAGE`), and the desk's own chrome fills most of the first."""
    keep_awake(vm)
    for _ in range(LISTING_READS):
        rows = read_listing(vm, query)
        if rows is not None:
            return rows
    raise VmError(f"the action listing changed on each of {LISTING_READS} reads while it was paged")


# How often a listing is read again from its first page when it moved on while paged, as it
# does while an app that just opened publishes its actions.
LISTING_READS = 10


def read_listing(vm: TiloVm, query: tuple[str, ...]) -> list[dict] | None:
    """One paged read of the listing, or `None` when it moved on between two pages
    (`stale_cursor`): a page past the first is cut from the listing the first was."""
    rows, cursor = [], None
    for _ in range(40):
        page = ["--cursor", cursor] if cursor else []
        try:
            listed = vm.control_json("actions", "list", *query, "--json", "--programmatic",
                                     "--wait=8000", "--limit", "256", *page)
        except VmError as exc:
            if cursor and "stale_cursor" in str(exc):
                return None
            raise
        rows.extend(listed.get("actions", []))
        cursor = listed.get("next_cursor")
        if not cursor:
            break
    return rows


def app_rows(vm: TiloVm, query: str, action: str) -> list[dict]:
    """The app actions named `action` the desktop lists: an app the desktop launches is listed
    under `app-actions`, a running app's instance under a provider of its own, naming its app
    (`application`) and its action (`operation`)."""
    def offered(row: dict) -> bool:
        if row.get("provider") in ("so.tilo.session", "applications"):
            return False
        named = row.get("operation") or str(row.get("id", "")).split("\x1f")[-1]
        return named == action and (row.get("provider") == "app-actions"
                                    or bool(row.get("application")))
    return [row for row in listed_actions(vm, *([query] if query else [])) if offered(row)]


def of_app(rows: list[dict], app: str) -> list[dict]:
    """The rows of `rows` that are `app`'s."""
    return [r for r in rows if r.get("application") == app or app in str(r.get("id", ""))]


def app_action(vm: TiloVm, query: str, action: str, args: dict | None = None, part: str = "") -> dict:
    """Invoke the app action `action` a live window offers (else its directory's row), as the
    agent-chat e2e does, and its answer once it succeeded."""
    rows = sorted((r for r in app_rows(vm, query, action)
                   if part in r["id"] or part == r.get("application")),
                  key=lambda row: row["id"].startswith("directory"))
    expect(rows, f"{vm.vm} offers no {action} (asked for {query!r})")
    argv = ["actions", "invoke", rows[0]["provider"], rows[0]["id"], "--wait=20000"]
    if args is not None:
        argv += ["--args-json", json.dumps(args)]
    answer = vm.control_json(*argv, timeout=90)
    state = (answer.get("receipt") or {}).get("update", {}).get("state")
    expect(state == "succeeded", f"{action} did not succeed on {vm.vm}: {json.dumps(answer)[:800]}")
    return answer


def record_part(row_id: str) -> str:
    parts = row_id.split("\x1f")
    return parts[-2] if len(parts) >= 2 else ""


CHROME = "desktop:chrome\x1f"


def open_app(vm: TiloVm, app: str) -> dict:
    """Open `app` in the shown strip, as the desk's own Open an app does."""
    return act(vm, "so.tilo.session", CHROME + "open-app", {"app": app}, wait=True)


def open_provider_settings(vm: TiloVm) -> None:
    """The model providers' settings: the app a person adds a provider and its key in."""
    if not app_rows(vm, "", "add-provider"):
        open_app(vm, "so.tilo.model-providers")
    if not wait_until(lambda: app_rows(vm, "", "add-provider"), 60, interval=1.0):
        keep_actions(run_of[vm.vm], vm.vm, "S03b")
        raise Failed(f"the model providers' window is open on {vm.vm}, but the desktop lists "
                           "none of its actions (an open app's own actions are not among them), so "
                           "no key can be saved without a person's hands")


def provider_part(vm: TiloVm) -> str | None:
    rows = app_rows(vm, PROVIDER_LABEL, "list-provider-models")
    return record_part(rows[0]["id"]) if rows else None


def add_provider(vm: TiloVm, key: str) -> str:
    """An OpenAI-compatible provider at the loopback endpoint, with `key` saved in the
    person's vault through Settings' own actions. Its record part."""
    open_provider_settings(vm)
    before = {r["id"] for r in app_rows(vm, "", "set-provider-details")}
    app_action(vm, "Add provider", "add-provider", {"preset": "openai-compatible"})
    new = wait_until(lambda: [r for r in app_rows(vm, "", "set-provider-details") if r["id"] not in before],
                     30, interval=1.0)
    expect(new, "Settings shows no row for the provider it added")
    part = record_part(new[0]["id"])
    app_action(vm, "", "set-provider-details", {"label": PROVIDER_LABEL,
               "endpoint": f"http://127.0.0.1:{MODEL_PORT}/v1", "model": PROVIDER_MODEL}, part)
    app_action(vm, "", "save-provider-key", {"key": key}, part)
    return part


def provider_answers(vm: TiloVm) -> dict:
    """Settings' Test connection for the provider: the gateway asks the endpoint for its models
    with the key it reads from the person's vault. Whether the endpoint saw the stored key."""
    open_provider_settings(vm)
    part = wait_until(lambda: provider_part(vm), 30, interval=1.0)
    expect(part, f"Settings on {vm.vm} lists no provider {PROVIDER_LABEL!r}")
    answer = app_action(vm, "", "list-provider-models", None, part)
    log = vm.root_exec("cat /run/e2e-model.log 2>/dev/null; true")
    expect("key=stored" in log, f"the endpoint never saw the stored key from {vm.vm}: {log.strip()!r}; "
                                f"the action answered {json.dumps(answer)[:600]}")
    expect(PROVIDER_MODEL in json.dumps(answer), f"Test connection listed no {PROVIDER_MODEL}: "
                                                 f"{json.dumps(answer)[:600]}")
    return {"endpoint_saw": sorted(set(log.split()))[:6]}


# -- the workspace and its desk -------------------------------------------------------


def workspaces(state: dict) -> list[dict]:
    return (state.get("workspace_tree") or {}).get("workspaces") or []


def held(state: dict, wid) -> bool:
    return wid in {row.get("workspace") for row in state.get("held_workspaces") or []}


def make_workspace(vm: TiloVm) -> dict:
    before = {w["id"] for w in workspaces(vm.state())}
    act(vm, "so.tilo.session", CHROME + "new-workspace", {"name": WORKSPACE}, wait=True)
    made = wait_until(lambda: next((w for w in workspaces(vm.state()) if w["id"] not in before), None),
                      30, interval=0.5)
    expect(made, "workspace.create made no workspace")
    select_workspace(vm, made["room"])
    expect(wait_until(lambda: held(vm.state(), made["id"]), 60, interval=1.0),
           f"box does not hold the workspace it made: {made}")
    return {"id": made["id"], "room": made["room"]}


def find_workspace(state: dict, room: str) -> dict | None:
    return next((w for w in workspaces(state) if w.get("room") == room), None)


def select_workspace(vm: TiloVm, room: str) -> None:
    state = vm.state()
    ws = find_workspace(state, room)
    expect(ws, f"{vm.vm} has no workspace {room} on its desk: "
               f"{[(w.get('room'), w.get('name')) for w in workspaces(state)]}")
    if (state.get("workspace_tree") or {}).get("active") != ws["id"]:
        act(vm, "so.tilo.session", CHROME + "switch-workspace", {"workspace": str(ws["id"])},
            wait=True)
        expect(wait_until(lambda: (vm.state().get("workspace_tree") or {}).get("active") == ws["id"],
                          15, interval=0.5), f"{vm.vm} did not show workspace {room}")


def groups_in(state: dict, room: str) -> list[dict]:
    ws = find_workspace(state, room)
    return [g for s in (ws or {}).get("strips") or [] for g in s.get("window_groups") or []]


# Apps every image carries, in the order a new window is opened from them: an app already
# showing in the workspace brings its window forward rather than opening another, so the next
# one is tried.
WINDOW_APPS = ("so.tilo.terminal", "so.tilo.notes", "so.tilo.calculator", "so.tilo.calendar",
               "so.tilo.chat")


def open_window(vm: TiloVm, ws: dict) -> object:
    """A new window on the workspace's desk, from the first app that opens one there. Its
    group id."""
    select_workspace(vm, ws["room"])
    before = {g["id"] for g in groups_in(vm.state(), ws["room"])}
    new = None
    for app in WINDOW_APPS:
        try:
            open_app(vm, app)
        except (Failed, VmError):
            continue
        new = wait_until(lambda: [g for g in groups_in(vm.state(), ws["room"])
                                  if g["id"] not in before], 30, interval=0.5)
        if new:
            break
    if not new:
        st = vm.state()
        where = {w.get("name"): [g["id"] for g in groups_in(st, w.get("room"))] for w in workspaces(st)}
        raise Failed(f"{vm.vm} opened no window in the workspace {ws['room']}: its desk holds {where}")
    return new[0]["id"]


# What the person's session says of its workspaces' sync, at debug: set on box before it is
# branched, so a and b inherit it. Read back only when a window does not travel.
SYNC_LOG = ("info,tilo_session::sync=debug,tilo_session::shared_desktop=debug,"
            "tilo_session::workspaces=debug,tilo_peer_state=debug,tilo::join_flow=debug,"
            "tilo_network_actor=debug")


def log_sync(vm: TiloVm) -> None:
    """Have the person's session log its sync at debug from its next start on."""
    vm.root_exec(
        "d=/etc/systemd/system/tilo-vm-session@me.service.d; mkdir -p $d && "
        f"printf '[Service]\\nEnvironment=RUST_LOG={SYNC_LOG}\\n' > $d/90-e2e-sync-log.conf && "
        "systemctl daemon-reload", timeout=60, check=False)


def notices(vm: TiloVm) -> str:
    """What the desk tells the person of changes another device would not take, as its state
    carries the notices."""
    import re
    said = re.compile(r"will not take|Your change stays|did not take|not a member|refus", re.I)
    found: list[str] = []

    def walk(node):
        if isinstance(node, dict):
            for value in node.values():
                walk(value)
        elif isinstance(node, list):
            for value in node:
                walk(value)
        elif isinstance(node, str) and said.search(node) and node not in found:
            found.append(node[:400])
    try:
        walk(vm.state())
    except (VmError, ValueError) as exc:
        return f"== notices: {exc}\n"
    return "== notices\n" + "\n".join(found[:20]) + "\n"


def sync_excerpt(vm: TiloVm) -> str:
    """The session's last words of its workspaces' sync this boot, unfiltered but for drawing."""
    text = vm.root_exec("journalctl -m --since \"$(uptime -s)\" --no-pager -o short-iso "
                        "SYSLOG_IDENTIFIER=tilo-session-me 2>&1 "
                        "| grep -av 'FRAME-LIFECYCLE\\|paint-stats\\|HEAP-MILESTONE\\|MESA\\|app_output\\|frames=' "
                        "| grep -a 'sync\\|shared_desktop\\|workspaces\\|peer_state\\|join_flow\\|network_actor\\|WARN\\|ERROR' "
                        "| grep -av 'has no live plane' "
                        "| tail -n 400 | cut -c1-500", timeout=120, check=False)
    return text or "(nothing)"


def session_excerpt(vm: TiloVm) -> str:
    """What the person's session said this boot of its identity, joining, its channels and its
    workspaces. Read with --merge: a branch renews its machine id after the journal opened its
    files under the old one, so plain journalctl finds nothing of its first boot."""
    import re
    try:
        text = vm.root_exec("journalctl -m --since \"$(uptime -s)\" --no-pager -o short-iso "
                            "SYSLOG_IDENTIFIER=tilo-session-me + _UID=$(id -u tds-me) "
                            "2>&1 | tail -n 4000",
                            timeout=120, check=False)
    except VmError as exc:
        return f"journalctl failed: {exc}"
    keep = re.compile(r"identity|recover|chain|join|channel|workspace|refus|admit|enter|host|"
                      r"certif|secret|vault|proof|sync|peer|transport|sibling|lock|ERROR|WARN", re.I)
    noise = ("FRAME-LIFECYCLE", "paint-stats", "HEAP-MILESTONE", "DEVICE-MEMORY", "frames=",
             "MESA", "app_output")
    lines = [l[:600] for l in text.splitlines() if not any(n in l for n in noise)]
    kept = [l for l in lines if keep.search(l)]
    return "\n".join((kept or lines)[-150:]) or f"(journalctl said nothing: {text[-300:]!r})"


def tabs_in(state: dict, room: str) -> dict:
    """The workspace's windows on this desk: group id to its tabs' titles."""
    return {g["id"]: [t.get("title") for t in g.get("tabs") or []] for g in groups_in(state, room)}


def edit_shows_on(run: Run, ws: dict, writer: str, reader: str, label: str, timeout: float = 300) -> dict:
    """A window `writer` opens on the workspace's desk shows on `reader`'s desk: a window
    group `reader` did not show before, the one the writer opened (by id, or by its tabs'
    titles where the desk names it differently there)."""
    w, r = run.vm(writer), run.vm(reader)
    select_workspace(r, ws["room"])
    before = tabs_in(r.state(), ws["room"])
    gid = open_window(w, ws)

    def arrived():
        # Both desks stay in use while the window travels: an idle desk locks, and a locked
        # one offers none of its actions to the next step.
        keep_awake(w)
        keep_awake(r)
        try:
            mine = tabs_in(w.state(), ws["room"]).get(gid)
            theirs = tabs_in(r.state(), ws["room"])
        except VmError:
            return None
        new = {g: t for g, t in theirs.items() if g not in before}
        return new if gid in new or (mine and mine in new.values()) else None

    seen = wait_until(arrived, timeout, interval=2.0)
    if not seen:
        run.write(f"{label}-{writer}-state.json", json.dumps(w.state(), indent=1))
        run.write(f"{label}-{reader}-state.json", json.dumps(r.state(), indent=1))
        run.write(f"{label}-{writer}-sync-session.txt", notices(w) + sync_excerpt(w))
        run.write(f"{label}-{reader}-sync-session.txt", notices(r) + sync_excerpt(r))
        def desk(vm):
            st = vm.state()
            return {"workspaces": [(w.get("room"), w.get("name"), len(groups_in(st, w.get("room"))))
                                   for w in workspaces(st)],
                    "held": st.get("held_workspaces")}
        raise Failed(f"desks: {writer} {desk(w)}; {reader} {desk(r)}. "
                     f"A window {writer} opened in the workspace (group {gid}, "
                     f"{tabs_in(w.state(), ws['room']).get(gid)}) did not show on {reader} within "
                     f"{timeout:.0f}s; {reader} shows {tabs_in(r.state(), ws['room'])}")
    return {"group": gid, "shown": {str(k): v for k, v in seen.items()}}


# -- branching ----------------------------------------------------------------------


def branch(facts: dict, run: Run) -> None:
    box = run.vm(SOURCE)
    out = box.vm_cmd("branch", SOURCE, *BRANCHES, "--stop", timeout=900)
    run.write("S04-branch.txt", out)
    for name in NAMES:
        plug_lan(run, name)
    listing = box.vm_cmd("list", timeout=120, check=False)
    run.write("S04-list.txt", listing)


def setup_shown(vm: TiloVm) -> list[str]:
    """The setup layer's controls, when setup or identity recovery stands over the desk."""
    try:
        targets = first_json(vm.run("control", "raw", '{"op":"list_targets"}', timeout=60, check=False))
    except VmError:
        return []
    names = [t.get("name") or "" for t in (targets.get("answer") or targets).get("targets") or []]
    if "Setup" not in names:
        return []
    return [n for n in names if len(n) < 80][:20]


def boot(facts: dict, run: Run, names: tuple[str, ...]) -> None:
    for name in names:
        run.vm(name).vm_cmd("start", name, timeout=900)
    # The person's session comes up at the desk: a branch or its source that opened identity
    # setup or recovery instead would ask the person for what the box already holds.
    over = {}
    for name in names:
        shown = desk_at_boot(run, name, "S05")
        if shown:
            over[name] = shown
        else:
            run.write(f"S05-{name}-session.txt", session_excerpt(run.vm(name)))
    for name in names:
        facts[f"lan_{name}"] = address_lan(run, name).strip()[-300:]
    # The sessions start again on the network the boxes now share, as on a box that came up
    # on it.
    for name in names:
        run.vm(name).run("slot", "restart", SLOT, timeout=300, slot=False)
        wait_session(run.vm(name))
    expect(not over, f"setup or identity recovery stood over the desk at first boot: {over}")


def judge_machine(facts: dict, run: Run, name: str, source: dict, others: dict) -> dict:
    m = machine_facts(run, name, f"S06-{name}")
    boot = keep_boot(run, name, "S06")
    facts["branch_unit"] = [l for l in boot.splitlines() if "tilo-device-key-init" in l or "branch" in l][-25:]
    facts["machine"] = {k: m.get(k) for k in ("UUID", "RECORD", "KEY-DIGEST", "DEVICE-PUBLIC",
                                               "CERT", "PENDING-LABEL", "JOURNAL", "BRANCH-UNIT")}
    problems = []
    if m.get("RECORD") != m.get("UUID"):
        problems.append(f"machine.bin {m.get('RECORD')} is not this machine's UUID {m.get('UUID')}")
    if name != SOURCE:
        if m.get("UUID") == source.get("UUID"):
            problems.append("the branch reports the source's product UUID")
        if m.get("KEY-DIGEST") == source.get("KEY-DIGEST"):
            problems.append("the branch kept the source's device key")
        if m.get("MACHINE-ID") == source.get("MACHINE-ID"):
            problems.append("the branch kept the source's /etc/machine-id")
        if set(m["HOST-KEY"]) & set(source["HOST-KEY"]):
            problems.append("the branch kept SSH host keys of the source's")
        if m.get("JOURNAL"):
            problems.append("the re-key journal is still there (a store was not re-sealed)")
    else:
        for key in ("KEY-DIGEST", "MACHINE-ID", "UUID"):
            if m.get(key) != source.get(key):
                problems.append(f"box's {key} changed across the branch")
        if m["HOST-KEY"] != source["HOST-KEY"]:
            problems.append("box's SSH host keys changed across the branch")
    for other, theirs in others.items():
        if theirs.get("KEY-DIGEST") == m.get("KEY-DIGEST"):
            problems.append(f"{name} and {other} share a device key")
        if theirs.get("MACHINE-ID") == m.get("MACHINE-ID"):
            problems.append(f"{name} and {other} share a machine id")
    if m.get("CERT") and source.get("CERT") and m.get("CERT") != source.get("CERT"):
        problems.append(f"{name} holds certificates of {m.get('CERT')}, the source of {source.get('CERT')}")
    expect(not problems, "; ".join(problems))
    return m


def judge_person(facts: dict, run: Run, name: str, source_person: dict, source_ident: dict,
                 devices: dict, key_digest: str | None, passwords: list[str] | None) -> dict:
    vm = run.vm(name)
    # The certificate a branch without its root in the system tree gets at the person's
    # next sign-in; this one holds it, so the boot issued it. Give the session a moment.
    def certified():
        q = person_facts(run, name, f"S07-{name}")
        return q if (q.get("identity") or {}).get("this_device") else None
    p = wait_until(certified, 120, interval=5.0) or person_facts(run, name, f"S07-{name}")
    ident = p.get("identity") or {}
    this = ident.get("this_device") or {}
    facts["identity"] = {"id": (ident.get("identity") or {}).get("id"), "this_device": this}
    facts["person"] = {k: p.get(k) for k in ("SECRET-DIGEST", "GIT-NAME", "SSH-KEY", "T-CONFIG",
                                             "STORE")}
    problems = []
    if (ident.get("identity") or {}).get("id") != (source_ident.get("identity") or {}).get("id"):
        problems.append(f"the person is {ident.get('identity')}, not the source's "
                        f"{source_ident.get('identity')}")
    if not this:
        problems.append(f"{name} holds no valid certificate for the person: {ident.get('next')}")
    else:
        mine = this.get("device")
        if devices.get(name, {}).get("DEVICE-PUBLIC") and mine != devices[name]["DEVICE-PUBLIC"]:
            problems.append(f"the certificate names device {mine}, this device's key is "
                            f"{devices[name]['DEVICE-PUBLIC']}")
        if name != SOURCE and mine == (source_ident.get("this_device") or {}).get("device"):
            problems.append("the branch's certificate is the source's")
    for key in ("SECRET-DIGEST", "GIT-NAME", "SSH-KEY", "T-CONFIG"):
        if p.get(key) != source_person.get(key):
            problems.append(f"{key}: {p.get(key)!r}, on the source {source_person.get(key)!r}")
    if key_digest:
        serve_model(vm, key_digest)
        try:
            facts["provider"] = provider_answers(vm)
        except (Failed, VmError) as exc:
            problems.append(f"the model provider's API key: {exc}")
    if passwords:
        try:
            open_passwords(vm)
            listed = wait_until(lambda: set(passwords) <= (held := password_items(vm)) and held,
                                60, interval=2.0)
            facts["passwords"] = "listed" if listed else sorted(password_items(vm))
            if not listed:
                problems.append(f"Passwords lists {sorted(password_items(vm))}, not the vault's "
                                f"{passwords}")
        except (Failed, VmError) as exc:
            problems.append(f"the passwords vault: {exc}")
    if not str(p.get("SSH-KEY-READABLE", "")).startswith("-----BEGIN OPENSSH PRIVATE KEY"):
        problems.append("the SSH private key is not readable as the person")
    expect(not problems, "; ".join(problems))
    return p


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--disk", type=Path, required=True)
    ap.add_argument("--commit")
    ap.add_argument("--tilo-vm", default="tilo-vm")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--taps", required=True, help="the tap devices of box, a and b on their network")
    ap.add_argument("--hold", type=int, default=0, help="seconds to keep the VMs up after the steps")
    ap.add_argument("--boot-only", action="store_true",
                    help="stop after the boxes' first boot and its checks (S01-S07)")
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    run = Run(args.tilo_vm, args.out, dict(zip(NAMES, args.taps.split(","))))
    meta = {"commit": args.commit, "started": dt.datetime.now(dt.UTC).isoformat(timespec="seconds"),
            "disk": str(args.disk)}
    try:
        steps(run, args)
    finally:
        status = finish(run, meta, args)
    return status


def steps(run: Run, args) -> None:
    run.facts = {"host_mem_available_mb": mem_available_mb()}
    # Three guests at once where the host has room for them; else b boots once box has
    # done its part, and is judged against the facts a and box left.
    together = mem_available_mb() >= 3 * GUEST_MB + HOST_MB
    run.step("S01", "box boots on this image and records its machine", boot_box, run, args.disk, GUEST_MB)
    if not run.passed("S01"):
        return
    run.step("S02", "a person signs in on box: the VM's person, certified by their root",
             sign_in, run)
    run.step("S03", "the person sets box up: a Secret Service secret, git, an SSH key", configure, run)
    run.step("S03b", "the person saves a model provider's API key through Settings", configure_provider, run)
    run.step("S03d", "the person saves a login in their passwords vault", configure_passwords, run)
    run.step("S03c", "the person makes a workspace on box and opens a window on its desk",
             configure_workspace, run)
    try:
        source = machine_facts(run, SOURCE, "S03-box-source")
        source_person = person_facts(run, SOURCE, "S03-box-source")
    except VmError as exc:
        run.step("S03d", "box's facts before the branch", lambda f: (_ for _ in ()).throw(exc))
        return
    source_ident = source_person.get("identity") or {}
    key_digest = next((s["facts"].get("api_key_digest") for s in run.steps
                       if s["id"] == "S03b" and s["status"] == "pass"), None)
    passwords = next((s["facts"].get("password_items") for s in run.steps
                      if s["id"] == "S03d" and s["status"] == "pass"), None)
    log_sync(run.vm(SOURCE))
    run.step("S03e", "box stopped and started again comes up at its desk", cold_boot, run)
    run.step("S04", "box is stopped and branched into a and b", branch, run)
    if not run.passed("S04"):
        return
    first = (SOURCE, *BRANCHES) if together else (SOURCE, "a")
    devices: dict[str, dict] = {}

    def judge(names: tuple[str, ...]) -> None:
        for name in names:
            others = {o: devices[o] for o in devices if o != name}
            m = run.step(f"S06-{name}", f"{name}: its own device key, machine record, machine id and "
                         "SSH host keys", judge_machine, run, name, source, others)
            devices[name] = m or machine_facts(run, name, f"S06-{name}")
        for name in names:
            run.step(f"S07-{name}", f"{name}: a valid certificate under the person's root, and the "
                     "person's secret, API key, SSH key and git config", judge_person, run, name,
                     source_person, source_ident, devices, key_digest, passwords)

    run.step("S05", f"{', '.join(first)} boot", boot, run, first)
    judge(first)
    if args.boot_only:
        return
    ws = next((s["facts"].get("workspace") for s in run.steps if s["id"] == "S03c"), None)
    if ws:
        run.step("S08", "an edit on a shows on box", lambda f: f.update(
            edit_shows_on(run, ws, "a", SOURCE, "S08")))
        run.step("S09", "an edit on box shows on a", lambda f: f.update(
            edit_shows_on(run, ws, SOURCE, "a", "S09")))
        run.step("S10", "box and a reboot", reboot, run, (SOURCE, "a"))
        run.step("S11", "after the reboot, an edit on a shows on box", lambda f: f.update(
            edit_shows_on(run, ws, "a", SOURCE, "S11")))
        run.step("S12", "after the reboot, an edit on box shows on a", lambda f: f.update(
            edit_shows_on(run, ws, SOURCE, "a", "S12")))
    if not together:
        run.step("S05-b", "b boots, with box stopped to make room", boot_later, run)
        judge(("b",))
    run.step("S13", "vm rm box leaves a and b bootable", remove_source, run)


def boot_later(facts: dict, run: Run) -> None:
    run.vm(SOURCE).vm_cmd("stop", SOURCE, timeout=600)
    boot(facts, run, ("b",))


def cold_boot(facts: dict, run: Run) -> None:
    """The first boot after the branch with no branch: box stopped as `--stop` stops it, and
    started. A fault of every cold boot shows here, apart from one of branching."""
    vm = run.vm(SOURCE)
    vm.vm_cmd("stop", SOURCE, timeout=600)
    vm.vm_cmd("start", SOURCE, timeout=900)
    shown = desk_at_boot(run, SOURCE, "S03e")
    facts["setup_shown"] = shown
    expect(not shown, f"box stopped and started, not branched, did not come up at its desk: {shown}")


def reboot(facts: dict, run: Run, names: tuple[str, ...]) -> None:
    for name in names:
        run.vm(name).vm_cmd("restart", name, timeout=900)
    over = {}
    for name in names:
        shown = desk_at_boot(run, name, "S10")
        if shown:
            over[name] = shown
        facts[f"lan_{name}"] = address_lan(run, name).strip()[-300:]
    expect(not over, f"setup or identity recovery stood over the desk after a reboot: {over}")


def remove_source(facts: dict, run: Run) -> None:
    run.vm(SOURCE).vm_cmd("rm", SOURCE, "--force", timeout=600)
    for name in BRANCHES:
        run.vm(name).vm_cmd("restart", name, timeout=900)
        wait_session(run.vm(name))
        m = machine_facts(run, name, f"S13-{name}")
        expect(m.get("RECORD") == m.get("UUID"), f"{name} after box was removed: {m}")


def finish(run: Run, meta: dict, args) -> int:
    if args.hold:
        print(f"holding the VMs for {args.hold}s", flush=True)
        time.sleep(args.hold)
    for name in reversed(list(run.vms)):
        try:
            run.vm(name).vm_cmd("rm", name, "--force", timeout=600, check=False)
        except VmError:
            pass
    meta.update(run.facts)
    meta["finished"] = dt.datetime.now(dt.UTC).isoformat(timespec="seconds")
    failed = [s for s in run.steps if s["status"] not in ("pass", "skip")]
    meta["overall"] = "fail" if failed or not run.steps else "pass"
    (args.out / "report.json").write_text(json.dumps({"meta": meta, "steps": run.steps}, indent=1))
    skipped = [s for s in run.steps if s["status"] == "skip"]
    lines = [f"# Branching a configured box: {meta['overall'].upper()}", "",
             f"commit {meta.get('commit')}, {len(run.steps) - len(failed) - len(skipped)}/"
             f"{len(run.steps)} steps pass, {len(skipped)} skipped (not scriptable)", ""]
    for s in run.steps:
        lines.append(f"- {s['id']} {s['status'].upper()} ({s['seconds']}s) {s['title']}")
        if s["detail"]:
            lines.append(f"  - {s['detail'][:1500]}")
    (args.out / "report.md").write_text("\n".join(lines) + "\n")
    return 0 if meta["overall"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
