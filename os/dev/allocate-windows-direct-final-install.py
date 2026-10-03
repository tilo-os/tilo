#!/usr/bin/env python3
"""Plan, allocate, and verify only Tilo's final partitions beside Windows.

`prepare` and `verify` are read-only. `execute` requires an exact reviewed
request digest, disk ID, recovery-key backup assertion tied to the current
BitLocker protector ID, native elevation, and a suspended BitLocker volume.
It may shrink only the reviewed C: partition and create only the three exact
Tilo partitions. The output plan records Windows-assigned GPT GUIDs for a
separate VM installation and guarded final content copy. It never formats a
partition, copies an image, or changes firmware.
"""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile

MIB = 1024**2
GIB = 1024**3
MIN_FREE_AFTER_SHRINK = 15 * GIB
REQUEST_SCHEMA = "tilo.windows-direct-allocation-request.v1"
ROLES = ("final-esp", "final-boot", "final-root")
MSR_GUID = "e3c9e316-0b5c-4db8-817d-f92df00215ae"
WINRE_GUID = "de94bba4-06d1-4d40-a16a-bfd50179d6ac"

_copy_path = Path(__file__).with_name("copy-windows-direct-final-install.py")
_spec = importlib.util.spec_from_file_location("tilo_windows_direct_copy", _copy_path)
direct = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(direct)
stage = direct.stage


def require(condition, message):
    if not condition:
        raise ValueError(message)


def guid(value):
    return stage.guid(value)


def normalized(rows):
    return stage.normalized_rows(rows)


def read_json(path):
    require(path.is_file() and not path.is_symlink(), f"missing regular file: {path}")
    value = json.loads(path.read_bytes())
    require(type(value) is dict, "request must be a JSON object")
    return value


def raw_disk_read(number):
    """Open native physical disk read-only for independent GPT validation."""
    require(platform.system() == "Windows", "raw disk inspection requires native Windows")
    import msvcrt
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    create = kernel.CreateFileW
    create.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32,
                       ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p]
    create.restype = ctypes.c_void_p
    handle = create(f"\\\\.\\PhysicalDrive{number}", 0x80000000, 3, None, 3, 0, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    return os.fdopen(msvcrt.open_osfhandle(handle, os.O_BINARY | os.O_RDONLY), "rb", buffering=0)


def windows_c_and_recovery():
    value = stage.ps_json(r'''$ErrorActionPreference='Stop'
$p=Get-Partition -DriveLetter C -ErrorAction Stop
$b=Get-BitLockerVolume -MountPoint C: -ErrorAction Stop
$ids=@($b.KeyProtector | Where-Object {$_.KeyProtectorType -eq 'RecoveryPassword'} |
  ForEach-Object {([string]$_.KeyProtectorId).Trim('{','}').ToLowerInvariant()})
[pscustomobject]@{disk=[int]$p.DiskNumber;number=[int]$p.PartitionNumber;
 partuuid=([string]$p.Guid).Trim('{','}').ToLowerInvariant();recovery_protector_ids=$ids;
 free_bytes=[long](Get-PSDrive -Name C -ErrorAction Stop).Free}|ConvertTo-Json -Depth 4 -Compress''')
    require(type(value) is dict and type(value.get("recovery_protector_ids")) is list,
            "Windows could not report C: recovery protectors")
    ids = [guid(item) for item in value["recovery_protector_ids"]]
    require(len(ids) >= 1 and len(set(ids)) == len(ids),
            "C: lacks a distinct BitLocker recovery-password protector")
    value["recovery_protector_ids"] = ids
    return value


def request_from_snapshot(snapshot, c_volume, root_gib):
    require(root_gib in (32, 45), "root size must be 32 or 45 GiB")
    disk = {key: snapshot[key] for key in (
        "number", "unique_id", "serial", "size_bytes", "logical_sector_size",
        "partition_style", "is_boot", "is_system", "is_offline", "is_read_only")}
    require(str(disk["unique_id"]).strip() and str(disk["serial"]).strip(),
            "Windows disk lacks durable unique ID or serial")
    require(stage.disk_matches({"windows_disk": disk}, snapshot),
            "snapshot is not the online Windows system disk")
    observed = normalized(snapshot["partitions"])
    require(len(observed) == 4 and len({r["partuuid"] for r in observed}) == 4,
            "expected exactly four original Windows partitions")
    require([r["number"] for r in observed] == [1, 2, 3, 4]
            and [r["type_guid"] for r in observed]
            == [stage.ESP_GUID, MSR_GUID, direct.WINDOWS_DATA_GUID, WINRE_GUID],
            "original disk is not the reviewed Windows EFI/MSR/C:/WinRE layout")
    previous = MIB
    for row in observed:
        require(row["offset_bytes"] >= previous
                and row["offset_bytes"] + row["size_bytes"] <= disk["size_bytes"],
                "original Windows partitions overlap or exceed disk")
        previous = row["offset_bytes"] + row["size_bytes"]
    require(c_volume.get("disk") == disk["number"], "C: is not on the reviewed disk")
    matches = [r for r in observed if r["number"] == c_volume.get("number")
               and r["partuuid"] == guid(c_volume.get("partuuid"))]
    require(len(matches) == 1 and matches[0]["type_guid"] == direct.WINDOWS_DATA_GUID,
            "C: does not match exactly one Windows data partition")
    require(type(c_volume.get("recovery_protector_ids")) is list
            and len(c_volume["recovery_protector_ids"]) >= 1,
            "C: has no recovery-password protector")
    recovery_ids = [guid(item) for item in c_volume["recovery_protector_ids"]]
    require(len(set(recovery_ids)) == len(recovery_ids), "duplicate BitLocker recovery protector ID")
    shrink_bytes = (root_gib + 3) * GIB
    c_before = matches[0]
    require(c_before["size_bytes"] > shrink_bytes, "C: is smaller than reviewed shrink")
    c_after_size = c_before["size_bytes"] - shrink_bytes
    start = c_before["offset_bytes"] + c_after_size
    end = c_before["offset_bytes"] + c_before["size_bytes"]
    require(start % MIB == 0 and end % MIB == 0
            and end < disk["size_bytes"] - 34 * disk["logical_sector_size"],
            "C: shrink extent is not safely MiB-aligned within GPT")
    preserve = [dict(r) for r in observed]
    for row in preserve:
        if row["partuuid"] == c_before["partuuid"]:
            row["size_bytes"] = c_after_size
        require(row["offset_bytes"] + row["size_bytes"] <= start
                or end <= row["offset_bytes"],
                "reviewed shrink overlaps another Windows partition")
    final = [
        {"role": "final-esp", "type_guid": stage.ESP_GUID,
         "offset_bytes": start, "size_bytes": 2 * GIB},
        {"role": "final-boot", "type_guid": stage.LINUX_GUID,
         "offset_bytes": start + 2 * GIB, "size_bytes": GIB},
        {"role": "final-root", "type_guid": stage.TILO_ROOT_GUID,
         "offset_bytes": start + 3 * GIB, "size_bytes": root_gib * GIB},
    ]
    return {"schema": REQUEST_SCHEMA, "read_only_request": True,
            "windows_disk": disk, "observed_partitions": observed,
            "preserve_partitions": preserve,
            "planned_windows_shrink": {"partition_number": c_before["number"],
                                       "original_size_bytes": c_before["size_bytes"],
                                       "new_size_bytes": c_after_size},
            "target_free_extent": {"offset_bytes": start, "size_bytes": shrink_bytes},
            "target_partitions": final,
            "bitlocker_recovery_protector_ids": recovery_ids,
            "minimum_c_free_after_shrink_bytes": MIN_FREE_AFTER_SHRINK}


def load_request(path):
    value = read_json(path)
    require(value.get("schema") == REQUEST_SCHEMA and value.get("read_only_request") is True,
            "unsupported direct allocation request")
    disk = value.get("windows_disk")
    observed = value.get("observed_partitions")
    shrink = value.get("planned_windows_shrink")
    require(type(disk) is dict and type(observed) is list and type(shrink) is dict,
            "allocation request is incomplete")
    c = {"disk": disk["number"], "number": shrink["partition_number"],
         "partuuid": next(r["partuuid"] for r in observed if r["number"] == shrink["partition_number"]),
         "recovery_protector_ids": value.get("bitlocker_recovery_protector_ids")}
    root_bytes = value["target_partitions"][2]["size_bytes"]
    require(type(root_bytes) is int and root_bytes % GIB == 0,
            "reviewed root size is invalid")
    recreated = request_from_snapshot({"partitions": observed, **disk}, c, root_bytes // GIB)
    require(value == recreated, "allocation request differs from its reviewed derivation")
    return value


def classify(request, snapshot):
    require(stage.disk_matches({"windows_disk": request["windows_disk"]}, snapshot),
            "Windows disk identity or online state changed")
    observed = normalized(request["observed_partitions"])
    preserve = normalized(request["preserve_partitions"])
    rows = normalized(snapshot["partitions"])
    if rows == observed:
        return "pre-shrink", []
    existing = {r["partuuid"]: r for r in rows}
    require(len(existing) == len(rows), "physical partition IDs are not unique")
    require(len({r["number"] for r in rows}) == len(rows),
            "physical partition numbers are not unique")
    # Windows numbers partitions by offset. Inserting Tilo before WinRE can
    # move WinRE from number 4 to 7 without changing its GPT identity/extent.
    preserved_keys = ("partuuid", "type_guid", "offset_bytes", "size_bytes")
    require(all(existing.get(r["partuuid"]) is not None and
                all(existing[r["partuuid"]][key] == r[key] for key in preserved_keys)
                for r in preserve),
            "preserved Windows partition differs from reviewed post-shrink map")
    additions = [r for r in rows if r["partuuid"] not in {p["partuuid"] for p in preserve}]
    additions.sort(key=lambda r: r["offset_bytes"])
    require(len(additions) <= 3 and len(rows) == 4 + len(additions),
            "physical partition map has unexpected additions")
    for actual, planned in zip(additions, request["target_partitions"], strict=False):
        require(actual["type_guid"] == planned["type_guid"]
                and actual["offset_bytes"] == planned["offset_bytes"]
                and actual["size_bytes"] == planned["size_bytes"],
                "created final partition differs from reviewed type or geometry")
    return ("complete" if len(additions) == 3 else
            "post-shrink" if len(additions) == 0 else f"partial-{len(additions)}"), additions


def check_gpt(snapshot, *, opener=None):
    disk = snapshot["number"]
    if opener is None:
        opener = raw_disk_read
    with opener(disk) as stream:
        _, raw_rows = direct.read_gpt(stream, snapshot["size_bytes"])
    require(direct.same_gpt_content(raw_rows, snapshot["partitions"]),
            "physical GPT differs from Windows partition snapshot")


def check_c_state(request, c):
    shrink = request["planned_windows_shrink"]
    require(c.get("disk") == request["windows_disk"]["number"]
            and c.get("number") == shrink["partition_number"],
            "C: is not the reviewed shrink partition")
    expected = next(r for r in request["preserve_partitions"] if r["number"] == shrink["partition_number"])
    require(guid(c.get("partuuid")) == expected["partuuid"], "C: partition GUID changed")
    require(set(c.get("recovery_protector_ids", [])) == set(request["bitlocker_recovery_protector_ids"]),
            "BitLocker recovery-password protectors changed")


def check_free_space(request, c, state):
    free = stage.integer(c.get("free_bytes"), "C: free bytes")
    shrink = request["target_free_extent"]["size_bytes"] if state == "pre-shrink" else 0
    require(free >= shrink + request["minimum_c_free_after_shrink_bytes"],
            "C: lacks the reviewed shrink plus 15 GiB operational headroom")


def check_supported_shrink(request):
    disk = request["windows_disk"]["number"]
    part = request["planned_windows_shrink"]["partition_number"]
    size = request["planned_windows_shrink"]["new_size_bytes"]
    state = stage.ps_json(rf'''$ErrorActionPreference='Stop'
$s=Get-PartitionSupportedSize -DiskNumber {disk} -PartitionNumber {part} -ErrorAction Stop
[pscustomobject]@{{min=[long]$s.SizeMin;max=[long]$s.SizeMax}}|ConvertTo-Json -Compress''')
    require(type(state) is dict and type(state.get("min")) is int
            and type(state.get("max")) is int and state["min"] <= state["max"],
            "Windows returned an invalid supported C: size range")
    require(size <= state["max"], "Windows reports C: smaller than the reviewed target")
    return state["min"] <= size


def c_ntfs_size():
    volume = stage.ps_json(r'''$ErrorActionPreference='Stop'
$v=Get-Volume -DriveLetter C -ErrorAction Stop
[pscustomobject]@{filesystem=[string]$v.FileSystem;size_bytes=[long]$v.Size}|ConvertTo-Json -Compress''')
    require(type(volume) is dict and str(volume.get("filesystem", "")).upper() == "NTFS"
            and type(volume.get("size_bytes")) is int and volume["size_bytes"] > 0,
            "C: is not a readable NTFS volume")
    return volume["size_bytes"]


def run_diskpart(commands):
    """One fresh DiskPart process per script; caller validates all resulting geometry."""
    descriptor, name = tempfile.mkstemp(prefix="tilo-exact-shrink-", suffix=".txt")
    try:
        with os.fdopen(descriptor, "w", encoding="ascii", newline="\r\n") as script:
            script.write("select volume C\n" + commands + "\nexit\n")
        result = subprocess.run(["diskpart.exe", "/s", name], capture_output=True,
                                text=True, timeout=1800)
        require(result.returncode == 0,
                f"DiskPart exited with {result.returncode}: {(result.stderr or result.stdout).strip()}")
        return result.stdout
    finally:
        Path(name).unlink(missing_ok=True)


def exact_diskpart_shrink(request):
    """Fallback for a stale Get-PartitionSupportedSize range, with exact checks."""
    disk = request["windows_disk"]["number"]
    shrink = request["target_free_extent"]["size_bytes"]
    require(shrink > 0 and shrink % MIB == 0, "DiskPart shrink is not whole MiB")
    amount_mib = shrink // MIB
    run_diskpart("shrink querymax")
    # QueryMax is advisory. Recheck the exact target immediately before the
    # modifying command; DiskPart's localized text is not a safety check.
    current = stage.snapshot(disk)
    state, _ = classify(request, current)
    require(state == "pre-shrink", "C: changed during DiskPart shrink preflight")
    check_gpt(current)
    c = windows_c_and_recovery()
    check_c_state(request, c)
    check_free_space(request, c, state)
    stage.windows_install_gate()
    before_ntfs = c_ntfs_size()
    require(before_ntfs <= request["planned_windows_shrink"]["original_size_bytes"],
            "C: NTFS size exceeds reviewed partition")
    run_diskpart(f"shrink desired={amount_mib} minimum={amount_mib}")
    current = stage.snapshot(disk)
    state, _ = classify(request, current)
    require(state == "post-shrink", "DiskPart did not produce the exact reviewed partition map")
    check_gpt(current)
    c = windows_c_and_recovery()
    check_c_state(request, c)
    after_ntfs = c_ntfs_size()
    require(before_ntfs - after_ntfs == shrink
            and after_ntfs <= request["planned_windows_shrink"]["new_size_bytes"],
            "DiskPart did not produce the exact reviewed NTFS size change")


def create_partition(request, index):
    disk = request["windows_disk"]["number"]
    row = request["target_partitions"][index]
    stage.ps_json(rf'''$ErrorActionPreference='Stop'
New-Partition -DiskNumber {disk} -Offset {row['offset_bytes']} -Size {row['size_bytes']} -GptType '{{{row['type_guid']}}}' -ErrorAction Stop|Out-Null
[pscustomobject]@{{created=$true}}|ConvertTo-Json -Compress''')


def complete_plan(request, additions):
    require(len(additions) == 3, "final partitions have not all been created")
    final = []
    for planned, actual in zip(request["target_partitions"], additions, strict=True):
        require(all(actual[key] == planned[key]
                    for key in ("type_guid", "offset_bytes", "size_bytes")),
                "Windows assigned unexpected final partition geometry")
        final.append({**planned, "number": actual["number"], "partuuid": actual["partuuid"]})
    return {"schema": direct.DIRECT_PLAN_SCHEMA, "read_only_plan": True,
            "windows_disk": request["windows_disk"],
            "observed_partitions": request["observed_partitions"],
            "preserve_partitions": request["preserve_partitions"],
            "planned_windows_shrink": request["planned_windows_shrink"],
            "target_free_extent": request["target_free_extent"],
            "target_partitions": final}


def request_from_allocated_snapshot(snapshot, c_volume, root_gib):
    """Recover the reviewed request from an exact completed allocation.

    This reconstructs the four-partition pre-shrink map only in memory. The
    resulting request must reproduce all seven live extents and GPT types
    through the same derivation and classifier used by `execute`.
    """
    rows = normalized(snapshot.get("partitions"))
    require(len(rows) == 7 and [r["type_guid"] for r in rows] == [
        stage.ESP_GUID, MSR_GUID, direct.WINDOWS_DATA_GUID,
        stage.ESP_GUID, stage.LINUX_GUID, stage.TILO_ROOT_GUID, WINRE_GUID,
    ], "expected the exact Windows plus three Tilo partition layout")
    require(root_gib in (32, 45), "root size must be 32 or 45 GiB")
    before = [dict(row) for row in (*rows[:3], rows[6])]
    before[2]["size_bytes"] += (root_gib + 3) * GIB
    before[3]["number"] = 4
    original = {**snapshot, "partitions": before}
    request = request_from_snapshot(original, c_volume, root_gib)
    state, additions = classify(request, snapshot)
    require(state == "complete", "live allocation differs from reconstructed request")
    return request, complete_plan(request, additions)


def prepare(args):
    c = windows_c_and_recovery()
    snapshot = stage.snapshot(c["disk"])
    request = request_from_snapshot(snapshot, c, args.root_gib)
    check_gpt(snapshot)
    stage.atomic_json(args.output, request)
    print(f"read-only allocation request: {args.output}")


def recover(args):
    """Read-only recovery when allocation completed but its request was lost."""
    require(args.request_output != args.plan_output, "request and plan paths must differ")
    c = windows_c_and_recovery()
    snapshot = stage.snapshot(c["disk"])
    request, plan = request_from_allocated_snapshot(snapshot, c, args.root_gib)
    check_gpt(snapshot)
    check_c_state(request, c)
    check_free_space(request, c, "complete")
    stage.atomic_json(args.request_output, request)
    stage.atomic_json(args.plan_output, plan)
    print(f"read-only recovered allocation: {args.request_output} {args.plan_output}")


def inspect(args):
    request = load_request(args.request)
    disk = request["windows_disk"]["number"]
    current = stage.snapshot(disk)
    state, additions = classify(request, current)
    check_gpt(current)
    c = windows_c_and_recovery()
    check_c_state(request, c)
    result = {"state": state, "disk_identity_matches": True,
              "gpt_matches_windows": True, "final_partitions": additions}
    if state == "complete":
        result["plan"] = complete_plan(request, additions)
        if args.output:
            stage.atomic_json(args.output, result["plan"])
    print(json.dumps(result, indent=2))


def execute(args):
    require(platform.system() == "Windows" and ctypes.windll.shell32.IsUserAnAdmin(),
            "physical allocation requires elevated native Windows")
    request_bytes = args.request.read_bytes()
    require(hashlib.sha256(request_bytes).hexdigest() == args.confirm_request_sha256,
            "allocation request SHA-256 differs from reviewed bytes")
    request = load_request(args.request)
    disk = request["windows_disk"]["number"]
    require(args.confirm_disk_id == request["windows_disk"]["unique_id"],
            "confirmed disk ID differs from reviewed disk")
    confirmed_protector = guid(args.confirm_recovery_protector_id)
    require(args.confirm_recovery_key_backed_up is True
            and confirmed_protector in request["bitlocker_recovery_protector_ids"],
            "matching BitLocker recovery key backup was not explicitly confirmed")
    current = stage.snapshot(disk)
    state, additions = classify(request, current)
    check_gpt(current)
    c = windows_c_and_recovery()
    check_c_state(request, c)
    require(confirmed_protector in c["recovery_protector_ids"],
            "confirmed BitLocker protector is no longer on C:")
    check_free_space(request, c, state)
    stage.windows_install_gate()
    if state == "pre-shrink":
        if check_supported_shrink(request):
            shrink = request["planned_windows_shrink"]
            stage.ps_json(rf'''$ErrorActionPreference='Stop'
Resize-Partition -DiskNumber {disk} -PartitionNumber {shrink['partition_number']} -Size {shrink['new_size_bytes']} -ErrorAction Stop|Out-Null
[pscustomobject]@{{resized=$true}}|ConvertTo-Json -Compress''')
        else:
            exact_diskpart_shrink(request)
        current = stage.snapshot(disk)
        state, additions = classify(request, current)
        require(state == "post-shrink", "Windows shrink did not produce exact reviewed map")
        check_gpt(current)
    while len(additions) < 3:
        c = windows_c_and_recovery()
        check_c_state(request, c)
        check_free_space(request, c, state)
        create_partition(request, len(additions))
        current = stage.snapshot(disk)
        state, additions = classify(request, current)
        require(state == ("complete" if len(additions) == 3 else f"partial-{len(additions)}"),
                "Windows did not create the exact next final partition")
        check_gpt(current)
    plan = complete_plan(request, additions)
    stage.atomic_json(args.output, plan)
    print(f"exact final partitions created and verified: {args.output}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare_command = commands.add_parser("prepare", help="read-only reviewed request from current Windows Disk0")
    prepare_command.add_argument("--root-gib", type=int, choices=(32, 45), required=True)
    prepare_command.add_argument("--output", type=Path, required=True)
    prepare_command.set_defaults(handler=prepare)
    recover_command = commands.add_parser("recover", help="read-only exact request and plan from completed allocation")
    recover_command.add_argument("--root-gib", type=int, choices=(32, 45), required=True)
    recover_command.add_argument("--request-output", type=Path, required=True)
    recover_command.add_argument("--plan-output", type=Path, required=True)
    recover_command.set_defaults(handler=recover)
    verify_command = commands.add_parser("verify", help="read-only GPT and partition-state check")
    verify_command.add_argument("--request", type=Path, required=True)
    verify_command.add_argument("--output", type=Path, help="write realized plan only when allocation is complete")
    verify_command.set_defaults(handler=inspect)
    execute_command = commands.add_parser("execute", help="shrink C: and allocate only exact reviewed Tilo partitions")
    execute_command.add_argument("--request", type=Path, required=True)
    execute_command.add_argument("--confirm-request-sha256", required=True)
    execute_command.add_argument("--confirm-disk-id", required=True)
    execute_command.add_argument("--confirm-recovery-protector-id", required=True)
    execute_command.add_argument("--confirm-recovery-key-backed-up", action="store_true")
    execute_command.add_argument("--output", type=Path, required=True)
    execute_command.set_defaults(handler=execute)
    args = parser.parse_args(argv)
    try:
        args.handler(args)
        return 0
    except (OSError, ValueError, KeyError, TypeError, StopIteration, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
