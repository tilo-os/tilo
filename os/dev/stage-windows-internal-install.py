#!/usr/bin/env python3
"""Execute or verify a reviewed Windows USB-free Tilo staging plan.

The executor is resumable: it accepts either the exact pre-shrink map, the
exact post-shrink map, or the latter plus both exact staging partitions.  It
never repairs, moves, deletes, or guesses around any other partition.
"""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import uuid

PLAN_SCHEMA = "tilo.windows-internal-install-plan.v1"
CHUNK = 8 * 1024 * 1024
ESP_GUID = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
TILO_ROOT_GUID = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab"


def fail(message):
    raise ValueError(message)


def integer(value, label, minimum=0):
    if type(value) is not int or value < minimum:
        fail(f"invalid {label}")
    return value


def guid(value):
    try:
        return str(uuid.UUID(str(value).strip().strip("{}")))
    except (ValueError, AttributeError) as error:
        raise ValueError(f"invalid GUID: {value!r}") from error


def load_plan(path: Path) -> dict:
    if path.is_symlink():
        fail("operation plan must not be a symlink")
    value = json.loads(path.read_bytes())
    if type(value) is not dict or value.get("schema") != PLAN_SCHEMA or value.get("read_only_plan") is not True:
        fail("unsupported operation plan")
    required = {"source", "windows_disk", "observed_partitions", "preserve_partitions", "planned_windows_shrink",
                "reviewed_extent", "target_free_extent", "target_partitions", "staging", "post_boot_reclaim"}
    if not required.issubset(value):
        fail("operation plan lacks required fields")
    disk = value["windows_disk"]
    if type(disk) is not dict or not str(disk.get("unique_id", "")).strip() or not str(disk.get("serial", "")).strip():
        fail("operation plan lacks durable disk identity")
    integer(disk.get("number"), "disk number")
    integer(disk.get("size_bytes"), "disk size", 1)
    seen = set()
    for collection in ("observed_partitions", "preserve_partitions"):
        rows = value[collection]
        if type(rows) is not list or not rows:
            fail(f"invalid {collection}")
        collection_seen = set()
        for row in rows:
            normalized = (integer(row.get("number"), "partition number", 1), guid(row.get("partuuid")),
                          guid(row.get("type_guid")), integer(row.get("offset_bytes"), "partition offset", 1),
                          integer(row.get("size_bytes"), "partition size", 1))
            if normalized[1] in collection_seen:
                fail("duplicate planned partition identity")
            collection_seen.add(normalized[1])
        seen.update(collection_seen)
    staging = value["staging"]
    if type(staging) is not list or [row.get("role") for row in staging] != ["staging-root", "staging-esp"]:
        fail("plan must stage root first and ESP last")
    if [guid(row.get("type_guid")) for row in staging] != [TILO_ROOT_GUID, ESP_GUID]:
        fail("staging partition types do not match root-then-ESP policy")
    for row in staging:
        guid(row.get("type_guid"))
        for field in ("offset_bytes", "size_bytes", "source_offset_bytes"):
            integer(row.get(field), f"staging {field}", 1)
    source = value["source"]
    if type(source) is not dict or not re.fullmatch(r"[0-9a-f]{64}", str(source.get("sha256", ""))):
        fail("invalid source binding")
    integer(source.get("size_bytes"), "source size", 1)
    reviewed = value["reviewed_extent"]
    target = value["target_free_extent"]
    reviewed_start = integer(reviewed.get("offset_bytes"), "reviewed offset", 1)
    reviewed_size = integer(reviewed.get("size_bytes"), "reviewed size", 1)
    target_start = integer(target.get("offset_bytes"), "target offset", 1)
    target_size = integer(target.get("size_bytes"), "target size", 1)
    final = value["target_partitions"]
    if type(final) is not list or [row.get("role") for row in final] != ["final-esp", "final-root"]:
        fail("plan lacks the exact final ESP/root pair")
    if [guid(row.get("type_guid")) for row in final] != [ESP_GUID, TILO_ROOT_GUID]:
        fail("final partition types do not match ESP/root policy")
    for row in final:
        guid(row.get("partuuid"))
        integer(row.get("offset_bytes"), "final partition offset", 1)
        integer(row.get("size_bytes"), "final partition size", 1)
    if (final[0]["offset_bytes"] != target_start
            or final[0]["offset_bytes"] + final[0]["size_bytes"] != final[1]["offset_bytes"]
            or final[1]["offset_bytes"] + final[1]["size_bytes"] != target_start + target_size):
        fail("final partitions do not exactly fill the target extent")
    root, esp = staging
    if (target_start != reviewed_start or target_start + target_size != root["offset_bytes"]
            or root["offset_bytes"] + root["size_bytes"] != esp["offset_bytes"]
            or esp["offset_bytes"] + esp["size_bytes"] != reviewed_start + reviewed_size):
        fail("target and staging extents do not exactly partition the reviewed extent")
    for row in staging:
        if row["source_offset_bytes"] + row["size_bytes"] > source["size_bytes"]:
            fail("staging source extent exceeds the installer image")
    for row in value["preserve_partitions"]:
        if (row["offset_bytes"] < reviewed_start + reviewed_size
                and reviewed_start < row["offset_bytes"] + row["size_bytes"]):
            fail("a preserved partition overlaps the reviewed extent")
    return value


def hash_stream(stream, offset, size):
    stream.seek(offset)
    digest = hashlib.sha256()
    remaining = size
    while remaining:
        data = stream.read(min(CHUNK, remaining))
        if not data:
            fail("short read while hashing an extent")
        digest.update(data)
        remaining -= len(data)
    return digest.hexdigest()


def hash_file(path: Path):
    with path.open("rb", buffering=0) as stream:
        return hash_stream(stream, 0, path.stat().st_size)


def copy_extent(source, target, source_offset, target_offset, size):
    source.seek(source_offset)
    target.seek(target_offset)
    digest = hashlib.sha256()
    remaining = size
    while remaining:
        data = source.read(min(CHUNK, remaining))
        if not data:
            fail("installer source ended during staging")
        written = target.write(data)
        if written != len(data):
            fail("short write while staging the installer")
        digest.update(data)
        remaining -= written
    target.flush()
    os.fsync(target.fileno())
    return digest.hexdigest()


def copy_and_verify_file(source_path: Path, target_path: Path, components: list[dict]):
    """File-backed equivalent of physical staging, used for deterministic tests."""
    results = []
    with source_path.open("rb", buffering=0) as source, target_path.open("r+b", buffering=0) as target:
        for component in components:
            digest = copy_extent(source, target, component["source_offset_bytes"],
                                 component["offset_bytes"], component["size_bytes"])
            readback = hash_stream(target, component["offset_bytes"], component["size_bytes"])
            if digest != readback:
                fail("staged extent readback differs from its source")
            results.append({"role": component["role"], "sha256": digest})
    return results


def ps_json(script):
    result = subprocess.run(["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script],
                            capture_output=True, text=True, timeout=180)
    if result.returncode:
        fail((result.stderr or result.stdout).strip() or "PowerShell storage command failed")
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ValueError("PowerShell returned invalid JSON") from error


def snapshot(disk_number):
    return ps_json(rf'''$ErrorActionPreference='Stop'
$d=Get-Disk -Number {disk_number} -ErrorAction Stop
$p=@(Get-Partition -DiskNumber {disk_number} -ErrorAction Stop | ForEach-Object {{
 [pscustomobject]@{{number=[int]$_.PartitionNumber;partuuid=([string]$_.Guid).Trim('{{','}}').ToLowerInvariant();type_guid=([string]$_.GptType).Trim('{{','}}').ToLowerInvariant();offset_bytes=[long]$_.Offset;size_bytes=[long]$_.Size}}
}})
[pscustomobject]@{{number=[int]$d.Number;unique_id=[string]$d.UniqueId;serial=[string]$d.SerialNumber;size_bytes=[long]$d.Size;logical_sector_size=[int]$d.LogicalSectorSize;partition_style=[string]$d.PartitionStyle;is_boot=[bool]$d.IsBoot;is_system=[bool]$d.IsSystem;is_offline=[bool]$d.IsOffline;is_read_only=[bool]$d.IsReadOnly;partitions=$p}}|ConvertTo-Json -Depth 5 -Compress''')


def normalized_rows(rows):
    return sorted([{"number": integer(row.get("number"), "partition number", 1),
                    "partuuid": guid(row.get("partuuid")), "type_guid": guid(row.get("type_guid")),
                    "offset_bytes": integer(row.get("offset_bytes"), "partition offset", 1),
                    "size_bytes": integer(row.get("size_bytes"), "partition size", 1)} for row in rows],
                  key=lambda row: row["offset_bytes"])


def disk_matches(plan, current):
    expected = plan["windows_disk"]
    return (current.get("number") == expected["number"] and current.get("unique_id") == expected["unique_id"]
            and str(current.get("serial", "")).strip() == expected["serial"]
            and current.get("size_bytes") == expected["size_bytes"]
            and current.get("logical_sector_size") == 512 and str(current.get("partition_style", "")).upper() == "GPT"
            and current.get("is_boot") is True and current.get("is_system") is True
            and current.get("is_offline") is False and current.get("is_read_only") is False)


def stage_rows(current_rows, plan):
    preserve = normalized_rows(plan["preserve_partitions"])
    current = normalized_rows(current_rows)
    expected_by_id = {row["partuuid"]: row for row in preserve}
    current_by_id = {row["partuuid"]: row for row in current}
    if len(current_by_id) != len(current):
        fail("current partition identities are not unique")
    if any(current_by_id.get(identity) != row for identity, row in expected_by_id.items()):
        fail("a preserved Windows partition differs from the reviewed plan")
    additions = [row for row in current if row["partuuid"] not in expected_by_id]
    if not additions and len(current) == len(preserve):
        return "absent", []
    if len(additions) != 2 or len(current) != len(preserve) + 2:
        fail("current partition map is neither the reviewed post-shrink map nor an exact staged map")
    additions.sort(key=lambda row: row["offset_bytes"])
    expected = sorted(plan["staging"], key=lambda row: row["offset_bytes"])
    for actual, wanted in zip(additions, expected, strict=True):
        if (actual["type_guid"] != guid(wanted["type_guid"])
                or actual["offset_bytes"] != wanted["offset_bytes"]
                or actual["size_bytes"] != wanted["size_bytes"]):
            fail("existing staging partition differs from the reviewed plan")
    return "present", additions


def resize_windows(plan):
    shrink = plan.get("planned_windows_shrink")
    if type(shrink) is not dict:
        return
    disk = plan["windows_disk"]["number"]
    part = integer(shrink.get("partition_number"), "shrink partition", 1)
    size = integer(shrink.get("new_size_bytes"), "shrink size", 1)
    ps_json(rf'''$ErrorActionPreference='Stop'
$supported=Get-PartitionSupportedSize -DiskNumber {disk} -PartitionNumber {part} -ErrorAction Stop
if([long]$supported.SizeMin -gt {size}L -or [long]$supported.SizeMax -lt {size}L){{throw 'reviewed Windows shrink is currently unsupported'}}
Resize-Partition -DiskNumber {disk} -PartitionNumber {part} -Size {size} -ErrorAction Stop|Out-Null
[pscustomobject]@{{resized=$true}}|ConvertTo-Json -Compress''')


def windows_install_gate():
    state = ps_json(r'''$ErrorActionPreference='Stop'
$b=Get-BitLockerVolume -MountPoint C: -ErrorAction Stop
$pagefiles=@(Get-CimInstance Win32_PageFileUsage -ErrorAction Stop | Where-Object {$_.Name -like 'C:*'})
[pscustomobject]@{volume_status=[int]$b.VolumeStatus;protection_status=[int]$b.ProtectionStatus;encryption_percentage=[int]$b.EncryptionPercentage;lock_status=[int]$b.LockStatus;system_pagefile_count=$pagefiles.Count}|ConvertTo-Json -Compress''')
    if (state.get("volume_status") != 1 or state.get("protection_status") != 0
            or state.get("encryption_percentage") != 100 or state.get("lock_status") != 0):
        fail("BitLocker must remain fully encrypted, unlocked, and explicitly suspended")
    if state.get("system_pagefile_count") != 0:
        fail("the C: pagefile is still active; reboot after disabling it before shrinking")
    return state


def create_staging(plan):
    disk = plan["windows_disk"]["number"]
    lines = ["$ErrorActionPreference='Stop'"]
    for row in plan["staging"]:
        lines.append(f"New-Partition -DiskNumber {disk} -Offset {row['offset_bytes']} -Size {row['size_bytes']} -GptType '{{{guid(row['type_guid'])}}}' -NoDefaultDriveLetter -ErrorAction Stop|Out-Null")
    lines.append("[pscustomobject]@{created=$true}|ConvertTo-Json -Compress")
    ps_json("\n".join(lines))


def open_physical_drive(number):
    if platform.system() != "Windows":
        fail("physical staging requires native Windows")
    import msvcrt
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    create_file = kernel32.CreateFileW
    create_file.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p,
                            ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p]
    create_file.restype = ctypes.c_void_p
    handle = create_file(f"\\\\.\\PhysicalDrive{number}", 0xC0000000, 0x3, None, 3, 0x80000000, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    descriptor = msvcrt.open_osfhandle(handle, os.O_BINARY | os.O_RDWR)
    return os.fdopen(descriptor, "r+b", buffering=0)


def atomic_json(path, value):
    path = path.resolve(strict=False)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write((json.dumps(value, indent=2, sort_keys=True) + "\n").encode())
            output.flush(); os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        try: os.unlink(temporary)
        except FileNotFoundError: pass


def development_backend():
    path = Path(__file__).resolve().parents[1] / "usr/libexec/tilo-install"
    data = path.read_bytes()
    if b"--internal-source)" not in data:
        fail("development backend lacks protected internal-source support")
    return path, data, hashlib.sha256(data).hexdigest()


def make_live_record(plan, staged, backend_sha256=None):
    if backend_sha256 is None:
        backend_sha256 = "0" * 64
    return {"schema": "tilo.internal-install-source.v1", "source_sha256": plan["source"]["sha256"],
            "development_backend": {"efi_path": "/EFI/tilo/dev/tilo-install", "sha256": backend_sha256},
            "disk": {key: plan["windows_disk"][key] for key in ("size_bytes", "unique_id", "serial")},
            "preserve_partitions": plan["preserve_partitions"],
            "target_free_extent": plan["target_free_extent"],
            "target_partitions": plan["target_partitions"], "staging": staged}


def make_live_runner(plan, staged, backend_sha256):
    by_role = {row["role"]: row for row in staged}
    final = {row["role"]: row for row in plan["target_partitions"]}
    expected = sorted(plan["preserve_partitions"] + [
        {"partuuid": row["partuuid"], "offset_bytes": row["offset_bytes"], "size_bytes": row["size_bytes"]}
        for row in staged], key=lambda row: row["offset_bytes"])
    expected_args = "\n".join(
        f'  --expect-partition "{row["partuuid"]}:{row["offset_bytes"]}:{row["size_bytes"]}"'
        for row in expected)
    text = f'''#!/bin/bash
set -euo pipefail
[ "$(id -u)" -eq 0 ] || {{ echo "run this from a root recovery shell" >&2; exit 1; }}
: "${{TILO_USER:?set TILO_USER to the lowercase login name}}"
TILO_HOSTNAME="${{TILO_HOSTNAME:-tilo}}"
TILO_FULL_NAME="${{TILO_FULL_NAME:-}}"
TILO_LOCALE="${{TILO_LOCALE:-de_DE.UTF-8}}"
TILO_TIMEZONE="${{TILO_TIMEZONE:-Europe/Berlin}}"
TILO_KEYMAP="${{TILO_KEYMAP:-de}}"
here="$(cd "$(dirname "$0")" && pwd)"
backend="$here/tilo-install"
printf '%s  %s\n' '{backend_sha256}' "$backend" | sha256sum -c -
runtime=/run/tilo-install-internal
install -m 0755 "$backend" "$runtime"
chcon --reference=/usr/libexec/tilo-install "$runtime" 2>/dev/null || true
source_root="$(readlink -f /dev/disk/by-partuuid/{by_role['staging-root']['partuuid']})"
source_esp="$(readlink -f /dev/disk/by-partuuid/{by_role['staging-esp']['partuuid']})"
disk="$(lsblk -srpno NAME,TYPE "$source_root" | awk '$2=="disk"{{print $1;exit}}')"
[ -b "$disk" ] && [ "$(lsblk -srpno NAME,TYPE "$source_esp" | awk '$2=="disk"{{print $1;exit}}')" = "$disk" ] \
  || {{ echo 'staging partitions do not share one physical disk' >&2; exit 1; }}
[ "$(blockdev --getsize64 "$disk")" = '{plan['windows_disk']['size_bytes']}' ] \
  || {{ echo 'physical disk size differs from the reviewed Windows plan' >&2; exit 1; }}
devnum="$(lsblk -dno MAJ:MIN "$disk" | tr -d '[:space:]')"
wwn="$(lsblk -dno WWN "$disk" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
serial="$(lsblk -dno SERIAL "$disk" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
ptuuid="$(lsblk -dno PTUUID "$disk" | tr -d '[:space:]')"
logical="$(blockdev --getss "$disk")"
[ -n "$wwn$serial" ] && [ -n "$ptuuid" ] || {{ echo 'disk lacks stable runtime identity' >&2; exit 1; }}
read -rsp 'Account password: ' account_password; echo
read -rsp 'Repeat account password: ' account_confirm; echo
[ "$account_password" = "$account_confirm" ] || {{ echo 'account passwords differ' >&2; exit 1; }}
read -rsp 'Disk unlock passphrase: ' disk_passphrase; echo
read -rsp 'Repeat disk unlock passphrase: ' disk_confirm; echo
[ "$disk_passphrase" = "$disk_confirm" ] || {{ echo 'disk passphrases differ' >&2; exit 1; }}
args=(
  --disk "$disk" --user "$TILO_USER" --hostname "$TILO_HOSTNAME"
  --full-name "$TILO_FULL_NAME" --locale "$TILO_LOCALE" --timezone "$TILO_TIMEZONE" --keymap "$TILO_KEYMAP"
  --filesystem btrfs --encrypt --pin --yes
  --expect-disk-size-bytes '{plan['windows_disk']['size_bytes']}' --expect-disk-devnum "$devnum"
  --expect-disk-ptuuid "$ptuuid" --expect-logical-sector-size "$logical"
  --guided-alongside --confirm-free-space "$disk"
  --free-start-bytes '{plan['target_free_extent']['offset_bytes']}' --free-size-bytes '{plan['target_free_extent']['size_bytes']}'
  --efi-start-bytes '{final['final-esp']['offset_bytes']}' --efi-size-bytes '{final['final-esp']['size_bytes']}'
  --root-start-bytes '{final['final-root']['offset_bytes']}' --root-size-bytes '{final['final-root']['size_bytes']}'
  --new-efi-partuuid '{final['final-esp']['partuuid']}' --new-root-partuuid '{final['final-root']['partuuid']}'
  --internal-source
  --internal-source-root-partuuid '{by_role['staging-root']['partuuid']}' --internal-source-root-size-bytes '{by_role['staging-root']['size_bytes']}'
  --internal-source-efi-partuuid '{by_role['staging-esp']['partuuid']}' --internal-source-efi-size-bytes '{by_role['staging-esp']['size_bytes']}'
{expected_args}
)
[ -z "$wwn" ] || args+=(--expect-disk-wwn "$wwn")
[ -z "$serial" ] || args+=(--expect-disk-serial "$serial")
[ -z "${{TILO_KEYVARIANT:-}}" ] || args+=(--keyvariant "$TILO_KEYVARIANT")
[ "${{TILO_FIDO2:-0}}" = 1 ] && args+=(--fido2)
printf '%s\n%s\n' "$account_password" "$disk_passphrase" | "$runtime" "${{args[@]}}"
unset account_password account_confirm disk_passphrase disk_confirm
'''
    return text.encode()


def publish_live_record(plan, record, backend_data, runner_data):
    disk = plan["windows_disk"]["number"]
    esp = next(row for row in record["staging"] if row["role"] == "staging-esp")
    partuuid = guid(esp["partuuid"])
    mounted = ps_json(rf'''$ErrorActionPreference='Stop'
$p=Get-Partition -DiskNumber {disk} -ErrorAction Stop | Where-Object {{([string]$_.Guid).Trim('{{','}}').ToLowerInvariant() -eq '{partuuid}'}}
if(@($p).Count -ne 1){{throw 'staging ESP identity is missing or ambiguous'}}
$assigned=$false
if(-not $p.DriveLetter){{$p|Add-PartitionAccessPath -AssignDriveLetter -ErrorAction Stop|Out-Null;$assigned=$true;$p=Get-Partition -DiskNumber {disk} -PartitionNumber $p.PartitionNumber -ErrorAction Stop}}
if(-not $p.DriveLetter){{throw 'Windows could not mount the staged ESP'}}
[pscustomobject]@{{drive=([string]$p.DriveLetter).ToUpperInvariant();partition_number=[int]$p.PartitionNumber;assigned=[bool]$assigned}}|ConvertTo-Json -Compress''')
    drive = str(mounted.get("drive", ""))
    if not re.fullmatch(r"[A-Z]", drive):
        fail("Windows returned an invalid staged-ESP drive letter")
    root = Path(f"{drive}:\\EFI\\tilo")
    path = root / "internal-stage.json"
    data = (json.dumps(record, indent=2, sort_keys=True) + "\n").encode()
    try:
        root.mkdir(parents=True, exist_ok=True)
        backend = root / "dev/tilo-install"
        backend.parent.mkdir(parents=True, exist_ok=True)
        backend.write_bytes(backend_data)
        if hashlib.sha256(backend.read_bytes()).hexdigest() != record["development_backend"]["sha256"]:
            fail("development installer backend failed staged-ESP readback")
        runner = root / "dev/run-internal-install.sh"
        runner.write_bytes(runner_data)
        if runner.read_bytes() != runner_data:
            fail("internal installer runner failed staged-ESP readback")
        temporary = root / ".internal-stage.json.tmp"
        temporary.write_bytes(data)
        os.replace(temporary, path)
        if path.read_bytes() != data:
            fail("staged ESP operation record failed readback")
    finally:
        if mounted.get("assigned") is True:
            ps_json(rf'''$ErrorActionPreference='Stop'
$p=Get-Partition -DiskNumber {disk} -PartitionNumber {integer(mounted.get('partition_number'), 'staging ESP partition number', 1)} -ErrorAction Stop
if($p.DriveLetter){{$p|Remove-PartitionAccessPath -AccessPath (([string]$p.DriveLetter)+':\') -ErrorAction Stop|Out-Null}}
[pscustomobject]@{{unmounted=$true}}|ConvertTo-Json -Compress''')
    return hashlib.sha256(data).hexdigest()


def execute(args):
    plan = load_plan(args.plan)
    expected_id = plan["windows_disk"]["unique_id"]
    if args.confirm_disk_id != expected_id:
        fail("--confirm-disk-id must exactly match the reviewed Windows disk")
    if not ctypes.windll.shell32.IsUserAnAdmin():
        fail("physical staging requires an elevated Administrator process")
    image = Path(plan["source"]["path"])
    if image.is_symlink() or not image.is_file() or image.stat().st_size != plan["source"]["size_bytes"]:
        fail("installer source identity changed")
    if hash_file(image) != plan["source"]["sha256"]:
        fail("installer source SHA-256 differs from the reviewed receipt")
    current = snapshot(plan["windows_disk"]["number"])
    if not disk_matches(plan, current):
        fail("Windows disk identity or state changed")
    windows_install_gate()
    observed = normalized_rows(plan["observed_partitions"])
    preserve = normalized_rows(plan["preserve_partitions"])
    rows = normalized_rows(current["partitions"])
    if rows == observed:
        resize_windows(plan)
        current = snapshot(plan["windows_disk"]["number"])
        if not disk_matches(plan, current) or normalized_rows(current["partitions"]) != preserve:
            fail("Windows partition map after shrink differs from the reviewed plan")
    state, additions = stage_rows(current["partitions"], plan)
    if state == "absent":
        create_staging(plan)
        current = snapshot(plan["windows_disk"]["number"])
        if not disk_matches(plan, current):
            fail("Windows disk identity changed after staging creation")
        state, additions = stage_rows(current["partitions"], plan)
        if state != "present":
            fail("staging partitions were not created exactly as reviewed")
    results = []
    with image.open("rb", buffering=0) as source, open_physical_drive(plan["windows_disk"]["number"]) as target:
        for row in plan["staging"]:
            digest = copy_extent(source, target, row["source_offset_bytes"], row["offset_bytes"], row["size_bytes"])
            readback = hash_stream(target, row["offset_bytes"], row["size_bytes"])
            if digest != readback:
                fail("physical staging readback differs from source")
            actual = next(item for item in additions if item["offset_bytes"] == row["offset_bytes"])
            results.append({"role": row["role"], "partuuid": actual["partuuid"], "sha256": digest,
                            "offset_bytes": row["offset_bytes"], "size_bytes": row["size_bytes"]})
    _backend_path, backend_data, backend_hash = development_backend()
    live_record = make_live_record(plan, results, backend_hash)
    runner_data = make_live_runner(plan, results, backend_hash)
    record_hash = publish_live_record(plan, live_record, backend_data, runner_data)
    receipt = {"schema": "tilo.windows-internal-stage.v1", "disk": plan["windows_disk"],
               "target_free_extent": plan["target_free_extent"], "staging": results,
               "source_sha256": plan["source"]["sha256"], "readback_verified": True,
               "live_record_sha256": record_hash,
               "live_runner_sha256": hashlib.sha256(runner_data).hexdigest()}
    atomic_json(args.output, receipt)
    print(f"staging complete and read back: {args.output}")


def verify(args):
    plan = load_plan(args.plan)
    current = snapshot(plan["windows_disk"]["number"])
    if not disk_matches(plan, current):
        fail("Windows disk identity or state changed")
    if normalized_rows(current["partitions"]) == normalized_rows(plan["observed_partitions"]):
        state, additions = "pre-shrink", []
    else:
        state, additions = stage_rows(current["partitions"], plan)
    print(json.dumps({"disk_identity_matches": True, "staging": state, "partitions": additions}, indent=2))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("verify", help="read-only check of the current Windows disk against a plan")
    check.add_argument("--plan", type=Path, required=True); check.set_defaults(handler=verify)
    stage = commands.add_parser("execute", help="perform the exact reviewed shrink and staging operation")
    stage.add_argument("--plan", type=Path, required=True)
    stage.add_argument("--confirm-disk-id", required=True)
    stage.add_argument("--output", type=Path, required=True); stage.set_defaults(handler=execute)
    args = parser.parse_args(argv)
    try:
        args.handler(args); return 0
    except (OSError, ValueError, json.JSONDecodeError, subprocess.SubprocessError) as error:
        print(f"error: {error}", file=sys.stderr); return 1


if __name__ == "__main__":
    raise SystemExit(main())
