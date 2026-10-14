#!/usr/bin/python3
"""Boot-side evidence for the composefs+UKI spike. Runs INSIDE the guest.

  ./os/dev/composefs/vm.sh ssh 'python3 -' < os/dev/composefs/guest-check.py

The question this exists to answer is narrow: does PCR 11 actually carry
`tilo.generation=`? "PCR 11 is non-zero" does not answer it — a UKI measures
several sections into PCR 11 and a non-zero value proves only that something
was measured. So the check is done by construction instead:

  1. read the .cmdline section straight out of the UKI on the ESP,
  2. hash it the way systemd-stub does,
  3. find that exact digest in the TPM event log's PCR 11 chain,
  4. replay the whole PCR 11 chain and confirm it folds to the live PCR 11.

If all four hold, the bytes containing `tilo.generation=` are provably part of
what PCR 11 is bound to, and a seal against PCR 11 is a seal against the
generation number.
"""

import glob
import hashlib
import json
import os
import re
import struct
import subprocess
import sys

PASS, FAIL, NA = [], [], []


def ok(msg):
    PASS.append(msg)
    print(f"[ OK ]   {msg}")


def no(msg):
    FAIL.append(msg)
    print(f"[FAIL]   {msg}")


def na(msg):
    NA.append(msg)
    print(f"[ n/a ]  {msg}")


def info(msg):
    print(f"[info]   {msg}")


def run(*args):
    try:
        return subprocess.run(
            args, capture_output=True, text=True, timeout=120
        ).stdout
    except Exception as exc:  # noqa: BLE001
        return f"<{exc}>"


def pe_sections(path):
    """Return {section_name: bytes} for a PE/COFF image."""
    with open(path, "rb") as fh:
        d = fh.read()
    pe = struct.unpack_from("<I", d, 0x3C)[0]
    nsec = struct.unpack_from("<H", d, pe + 6)[0]
    optsz = struct.unpack_from("<H", d, pe + 20)[0]
    base = pe + 24 + optsz
    out = {}
    for i in range(nsec):
        o = base + i * 40
        name = d[o : o + 8].rstrip(b"\0").decode(errors="replace")
        vsz = struct.unpack_from("<I", d, o + 8)[0]
        rsz = struct.unpack_from("<I", d, o + 16)[0]
        raw = struct.unpack_from("<I", d, o + 20)[0]
        out[name] = d[raw : raw + min(vsz, rsz) if rsz else raw + vsz]
    return out


print("=== composefs + UKI: boot-side evidence ===\n")

# --- the command line --------------------------------------------------------
cmdline = open("/proc/cmdline").read().strip()
info(f"/proc/cmdline: {cmdline}")

m = re.search(r"(?:^|\s)tilo\.generation=(\d+)(?:\s|$)", cmdline)
gen = m.group(1) if m else None
if gen:
    ok(f"tilo.generation={gen} is on the kernel command line")
else:
    no("no tilo.generation= on the kernel command line")

if os.path.exists("/usr/lib/tilo/generation"):
    file_gen = open("/usr/lib/tilo/generation").read().strip()
    if file_gen == gen:
        ok(f"/usr/lib/tilo/generation agrees with the command line ({file_gen})")
    else:
        no(f"/usr/lib/tilo/generation is {file_gen} but the command line says {gen}")
else:
    no("/usr/lib/tilo/generation is missing")

# Tilo's hardening kargs have to survive into the sealed command line too, or the
# UKI path silently drops the hardening the ostree path applies at deploy time.
HARDENING = [
    "init_on_alloc=1",
    "lockdown=integrity",
    "slab_nomerge",
    "randomize_kstack_offset=on",
    "page_alloc.shuffle=1",
    "vsyscall=none",
    "systemd.import_credentials=no",
]
missing = [k for k in HARDENING if k not in cmdline.split()]
if missing:
    no(f"hardening kargs missing from the command line: {' '.join(missing)}")
else:
    ok(f"all {len(HARDENING)} hardening kargs are on the command line")

cfs = re.search(r"(?:^|\s)composefs=([0-9a-f]+)", cmdline)
if cfs:
    ok(f"composefs={cfs.group(1)[:16]}… ({len(cfs.group(1))} hex chars) is on the command line")
else:
    no("no composefs= on the command line — this is not a composefs boot")

# --- is this actually the composefs backend? ---------------------------------
# Where the physical root is mounted depends on how the initramfs handed over, so
# look in each place it can be rather than assuming one.
STATE_DIRS = ["/sysroot/state/deploy", "/state/deploy", "/run/host/state/deploy"]
state_dir = next((d for d in STATE_DIRS if os.path.isdir(d)), None)
if state_dir:
    deploys = sorted(os.listdir(state_dir))
    ok(f"composefs state dir present: {len(deploys)} deployment(s) under {state_dir}")
    for d in deploys:
        marker = " <- booted" if cfs and d == cfs.group(1) else ""
        info(f"  {d[:24]}…{marker}")
    if cfs and cfs.group(1) in deploys:
        ok("the booted deployment directory is named by the digest in the command line")
    elif cfs:
        no("no deployment directory matches the composefs digest on the command line")
else:
    no(f"none of {', '.join(STATE_DIRS)} exists — not a composefs-backend system")

# A composefs install still creates an /ostree directory, so its mere presence
# proves nothing. An ostree *repository* has an objects store; that is the thing
# whose absence says this is not the ostree backend.
ostree_repos = [
    d for d in ("/ostree/repo/objects", "/sysroot/ostree/repo/objects")
    if os.path.isdir(d)
]
if ostree_repos:
    no(f"an ostree object store exists ({ostree_repos[0]}) — this is the ostree backend")
else:
    ok("no ostree object store — not the ostree backend")

# --- UEFI + the stub --------------------------------------------------------
if os.path.isdir("/sys/firmware/efi"):
    ok("booted via UEFI")
else:
    no("not a UEFI boot")

stub = glob.glob("/sys/firmware/efi/efivars/StubPcrKernelImage-*")
if stub:
    ok("systemd-stub reported measuring the kernel image (StubPcrKernelImage)")
else:
    no("no StubPcrKernelImage EFI variable — not booted as a UKI")

loader = glob.glob("/sys/firmware/efi/efivars/LoaderEntrySelected-*")
if loader:
    raw = open(loader[0], "rb").read()[4:]
    info(f"LoaderEntrySelected: {raw.decode('utf-16-le', 'replace').rstrip(chr(0))}")

# --- the UKI on the ESP ------------------------------------------------------
ukis = sorted(glob.glob("/boot/EFI/Linux/**/*.efi", recursive=True))
uki_cmdline = None
if not ukis:
    no("no UKI under /boot/EFI/Linux")
else:
    info(f"UKIs on the ESP: {', '.join(ukis)}")
    for u in ukis:
        secs = pe_sections(u)
        c = secs.get(".cmdline", b"").rstrip(b"\0").decode(errors="replace").strip()
        info(f"  {u}: sections {' '.join(sorted(secs))}")
        info(f"  {u}: .cmdline = {c}")
        if cfs and f"composefs={cfs.group(1)}" in c:
            uki_cmdline = secs[".cmdline"]
            ok(f"{u} is the UKI this boot came from (its composefs digest matches)")
    if uki_cmdline is None:
        # Fall back to the single UKI present, so the measurement check can
        # still run and say something.
        secs = pe_sections(ukis[0])
        uki_cmdline = secs.get(".cmdline")
        no("could not match a UKI on the ESP to the running command line")

# --- PCRs -------------------------------------------------------------------
have_tpm = os.path.exists("/dev/tpmrm0") or os.path.exists("/dev/tpm0")
if not have_tpm:
    na("no TPM — measured boot does not apply")
else:
    ok("a TPM is present")
    live = {}
    out = run("tpm2_pcrread", "sha256:4,7,11")
    for line in out.splitlines():
        mm = re.match(r"\s*(\d+)\s*:\s*0x([0-9A-Fa-f]+)", line)
        if mm:
            live[int(mm.group(1))] = mm.group(2).lower()
    for pcr in (4, 7, 11):
        v = live.get(pcr)
        if not v:
            no(f"PCR {pcr} could not be read")
        elif v == "0" * 64:
            no(f"PCR {pcr} is all zeroes")
        else:
            ok(f"PCR {pcr} = {v}")

    # --- the decisive check: is the command line IN PCR 11? -----------------
    log = "/sys/kernel/security/tpm0/binary_bios_measurements"
    if not os.path.exists(log):
        no(f"{log} is absent — the event log cannot be replayed")
    elif uki_cmdline is None:
        no("no UKI .cmdline section to hash")
    else:
        raw = run("tpm2_eventlog", log)
        # tpm2_eventlog emits YAML. Rather than depend on a YAML module being in
        # the image, pull out (PCRIndex, sha256 digest) pairs in log order:
        # an event opens with "- EventNum:", carries a PCRIndex, and lists one
        # digest per bank — only the sha256 one is wanted. EV_NO_ACTION events
        # have a bare top-level Digest with no AlgorithmId and are never
        # extended, so gating on AlgorithmId also excludes them.
        #
        # This parser was checked by replaying every PCR on the build host and
        # comparing against tpm2_pcrread: all eleven matched.
        events = []
        pcr = None
        want_digest = False
        for line in raw.splitlines():
            s = line.strip()
            if s.startswith("- EventNum:"):
                pcr, want_digest = None, False
            m = re.match(r"^-?\s*PCRIndex:\s*(\d+)$", s)
            if m:
                pcr = int(m.group(1))
                continue
            if re.match(r"^-?\s*AlgorithmId:\s*sha256$", s):
                want_digest = True
                continue
            if want_digest:
                m = re.match(r'^Digest:\s*"([0-9a-fA-F]{64})"$', s)
                if m:
                    if pcr is not None:
                        events.append((pcr, m.group(1).lower()))
                    want_digest = False

        chain = [d for p, d in events if p == 11]
        info(f"PCR 11 event chain: {len(chain)} measurement(s)")

        # systemd-stub hashes the raw section bytes. Try the plausible framings
        # and report which one the log agrees with.
        candidates = {
            "section bytes as sized": uki_cmdline,
            "section bytes, trailing NULs stripped": uki_cmdline.rstrip(b"\0"),
            "NUL-terminated": uki_cmdline.rstrip(b"\0") + b"\0",
            "UTF-16LE of the text": uki_cmdline.rstrip(b"\0").decode().encode("utf-16-le"),
            "UTF-16LE, NUL-terminated": (
                uki_cmdline.rstrip(b"\0").decode() + "\0"
            ).encode("utf-16-le"),
        }
        matched = None
        for label, blob in candidates.items():
            h = hashlib.sha256(blob).hexdigest()
            if h in chain:
                matched = (label, h)
                break
        if matched:
            ok(
                f"the UKI's .cmdline is measured into PCR 11 "
                f"(sha256 {matched[1][:16]}… found in the PCR 11 chain; framing: {matched[0]})"
            )
            txt = uki_cmdline.rstrip(b"\0").decode(errors="replace")
            if gen and f"tilo.generation={gen}" in txt:
                ok(
                    f"PCR 11 CARRIES tilo.generation={gen} — the measured bytes "
                    f"contain it"
                )
            else:
                no("the measured .cmdline does not contain tilo.generation=")
        else:
            no("the UKI's .cmdline digest is NOT in the PCR 11 event chain")
            for label, blob in candidates.items():
                info(f"  tried {label}: {hashlib.sha256(blob).hexdigest()[:24]}…")
            info(f"  chain: {[c[:16] for c in chain]}")

        # Replay: fold the firmware chain, then systemd's own runtime extensions.
        #
        # PCR 11 does not stop growing when the stub is done: systemd extends it
        # once per boot phase (enter-initrd, leave-initrd, sysinit, ready) and
        # those events are NOT in the firmware event log — they are recorded in
        # /run/log/systemd/tpm2-measure.log instead. Folding only the firmware log
        # therefore reproduces an intermediate value, not the live register, and
        # reporting that as a mismatch would be wrong.
        acc = bytes(32)
        for d in chain:
            acc = hashlib.sha256(acc + bytes.fromhex(d)).digest()
        firmware_only = acc.hex()

        runtime = []
        mlog = "/run/log/systemd/tpm2-measure.log"
        if os.path.exists(mlog):
            for line in open(mlog):
                # RFC 7464 JSON text sequence: every record is prefixed with an
                # ASCII record separator (0x1e), which json.loads rejects with
                # "Expecting value: line 1 column 1".
                try:
                    ev = json.loads(line.lstrip("\x1e").strip())
                except ValueError:
                    continue
                if ev.get("pcr") != 11:
                    continue
                for dg in ev.get("digests", []):
                    if dg.get("hashAlg") == "sha256":
                        runtime.append((dg["digest"].lower(),
                                        (ev.get("content") or {}).get("string", "?")))
        for d, label in runtime:
            acc = hashlib.sha256(acc + bytes.fromhex(d)).digest()
        if runtime:
            info(
                f"systemd extended PCR 11 {len(runtime)} more time(s) at runtime: "
                + ", ".join(label for _, label in runtime)
            )

        if live.get(11) == acc.hex():
            ok(
                f"replaying PCR 11 reproduces the live register exactly "
                f"({len(chain)} firmware + {len(runtime)} runtime measurements)"
            )




        else:
            no(
                f"PCR 11 replay mismatch: firmware-only {firmware_only[:16]}…, "
                f"with runtime {acc.hex()[:16]}…, live {(live.get(11) or '?')[:16]}…"
            )

# --- bootc's own view --------------------------------------------------------
st = run("bootc", "status", "--json")
try:
    j = json.loads(st)
    spec = (j.get("status") or {}).get("booted") or {}
    img = ((spec.get("image") or {}).get("image") or {}).get("image")
    info(f"bootc booted image: {img}")
    info(f"bootc backend: {(j.get('status') or {}).get('backend', '<unset>')}")
    staged = (j.get("status") or {}).get("staged")
    rollback = (j.get("status") or {}).get("rollback")
    info(f"staged: {'yes' if staged else 'no'}   rollback: {'yes' if rollback else 'no'}")
except Exception:  # noqa: BLE001
    info("bootc status --json: " + run("bootc", "status").replace("\n", " | ")[:400])

# --- fs-verity / sealing ----------------------------------------------------
vt = run("findmnt", "-nro", "SOURCE,FSTYPE,OPTIONS", "/")
info(f"/ is {vt.strip()}")

print()
print(f"{len(PASS)} passed, {len(FAIL)} failed, {len(NA)} not applicable")
sys.exit(1 if FAIL else 0)
