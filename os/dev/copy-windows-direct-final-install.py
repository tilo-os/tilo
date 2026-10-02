#!/usr/bin/env python3
"""Copy a VM-installed Tilo's three final partitions onto a reviewed Windows disk.

The caller creates the empty final partitions first and records their actual
Windows-assigned GPT IDs in the reviewed plan. Execute writes only the final
ESP, /boot, and LUKS-root content extents. Only seven-partition direct plans
are accepted.
The direct route does not create staging partitions on the physical disk or
change UEFI firmware state. A matching source receipt binds every final extent
to a full SHA-256 digest of the VM-installed raw image, a checked guest proof,
the source ESP UKI, and CI-signed index and SLSA release provenance.
Successful execution is not proof that the guest installed the verified CI
image and does not make the disk bootable; guest provenance and EFI BootNext
must be checked separately before reporting a completed physical install.
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
import re
import struct
import subprocess
import sys
import uuid
import zlib

CHUNK = 8 * 1024 * 1024
SECTOR = 512
GIB = 1024**3
SCHEMA = "tilo.windows-direct-final-source.v1"
DIRECT_PLAN_SCHEMA = "tilo.windows-direct-final-plan.v1"
ROLES = ("final-esp", "final-boot", "final-root")
WINDOWS_DATA_GUID = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7"

_stage_path = Path(__file__).with_name("windows-direct-common.py")
_spec = importlib.util.spec_from_file_location("tilo_windows_direct_common", _stage_path)
stage = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(stage)
_provenance_path = Path(__file__).with_name("windows-direct-provenance.py")
_provenance_spec = importlib.util.spec_from_file_location("tilo_windows_direct_provenance", _provenance_path)
provenance = importlib.util.module_from_spec(_provenance_spec)
_provenance_spec.loader.exec_module(provenance)
_dev_spec = importlib.util.spec_from_file_location(
    "tilo_windows_direct_dev_admission", Path(__file__).with_name("windows-direct-dev-admission.py"))
development = importlib.util.module_from_spec(_dev_spec)
_dev_spec.loader.exec_module(development)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(value):
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value), "invalid SHA-256")
    return value


def integer(value, label, minimum=0):
    return stage.integer(value, label, minimum)


def load_json(path):
    require(not path.is_symlink() and path.is_file(), f"missing regular file: {path}")
    value = json.loads(path.read_bytes())
    require(type(value) is dict, f"invalid JSON object: {path}")
    return value


def normalized(rows):
    return stage.normalized_rows(rows)


def load_plan(path):
    value = load_json(path)
    require(value.get("schema") == DIRECT_PLAN_SCHEMA and value.get("read_only_plan") is True,
            "unsupported Windows direct-install plan")
    disk = value.get("windows_disk")
    require(type(disk) is dict and stage.integer(disk.get("number"), "disk number") >= 0
            and stage.integer(disk.get("size_bytes"), "disk size", 1) % SECTOR == 0
            and disk.get("logical_sector_size") == SECTOR
            and disk.get("partition_style") == "GPT"
            and disk.get("is_boot") is True and disk.get("is_system") is True
            and disk.get("is_offline") is False and disk.get("is_read_only") is False
            and isinstance(disk.get("unique_id"), str) and disk["unique_id"]
            and isinstance(disk.get("serial"), str) and disk["serial"],
            "invalid Windows disk identity")
    observed = normalized(value.get("observed_partitions"))
    preserve = normalized(value.get("preserve_partitions"))
    require(len(observed) == len(preserve) == 4, "direct plan requires four preserved Windows partitions")
    for set_name, rows in (("observed", observed), ("preserved", preserve)):
        require(len({r["number"] for r in rows}) == 4
                and len({r["partuuid"] for r in rows}) == 4,
                f"{set_name} Windows identities are not unique")
        previous = 1024 * 1024
        for row in rows:
            require(row["offset_bytes"] % SECTOR == 0 and row["size_bytes"] % SECTOR == 0
                    and row["offset_bytes"] >= previous
                    and row["offset_bytes"] + row["size_bytes"] <= disk["size_bytes"],
                    f"{set_name} Windows partitions overlap or exceed disk")
            previous = row["offset_bytes"] + row["size_bytes"]
    shrink = value.get("planned_windows_shrink")
    for before, after in zip(observed, preserve, strict=True):
        require(all(before[key] == after[key]
                    for key in ("number", "partuuid", "type_guid", "offset_bytes")),
                "preserved Windows partition identity or location changed")
        if before["size_bytes"] != after["size_bytes"]:
            require(type(shrink) is dict
                    and shrink.get("partition_number") == before["number"]
                    and shrink.get("original_size_bytes") == before["size_bytes"]
                    and shrink.get("new_size_bytes") == after["size_bytes"]
                    and after["size_bytes"] < before["size_bytes"],
                    "Windows shrink differs from reviewed plan")
    if shrink is not None:
        require(type(shrink) is dict and sum(
            before["size_bytes"] != after["size_bytes"]
            for before, after in zip(observed, preserve, strict=True)) == 1,
            "reviewed Windows shrink is missing or ambiguous")
    else:
        require(observed == preserve, "preserved Windows partition changed without a shrink")
    extent = value.get("target_free_extent")
    require(type(extent) is dict, "direct plan lacks final extent")
    start = integer(extent.get("offset_bytes"), "final extent offset", 1)
    size = integer(extent.get("size_bytes"), "final extent size", 1)
    require(start % (1024 * 1024) == 0 and size % (1024 * 1024) == 0
            and start + size <= disk["size_bytes"], "invalid final extent geometry")
    if shrink is not None:
        before = next(r for r in observed if r["number"] == shrink["partition_number"])
        after = next(r for r in preserve if r["number"] == shrink["partition_number"])
        require(after["offset_bytes"] + after["size_bytes"] == start
                and before["offset_bytes"] + before["size_bytes"] == start + size,
                "final extent is not exactly the reviewed Windows shrink")
    final = value.get("target_partitions")
    require(type(final) is list and [r.get("role") for r in final] == list(ROLES),
            "direct plan lacks exact final ESP/boot/root roles")
    require([stage.guid(r.get("type_guid")) for r in final]
            == [stage.ESP_GUID, stage.LINUX_GUID, stage.TILO_ROOT_GUID],
            "direct plan final partition types differ")
    checked_final = normalized(final)
    require([r["offset_bytes"] for r in checked_final]
            == [start, start + 2 * GIB, start + 3 * GIB]
            and [r["size_bytes"] for r in checked_final[:2]] == [2 * GIB, GIB]
            and checked_final[2]["size_bytes"] >= 32 * GIB
            and checked_final[2]["offset_bytes"] + checked_final[2]["size_bytes"] == start + size,
            "direct plan final geometry differs from 2 GiB ESP, 1 GiB boot, and >=32 GiB root")
    for existing in preserve:
        require(existing["offset_bytes"] + existing["size_bytes"] <= start
                or start + size <= existing["offset_bytes"],
                "direct final extent overlaps preserved Windows partition")
    require(len({r["partuuid"] for r in preserve + checked_final}) == 7,
            "direct plan partition GUIDs are not unique")
    return value


def expected_rows(plan, stage_receipt, current_rows):
    require(stage_receipt is None, "direct final copy cannot include staging receipt")
    rows = normalized(current_rows)
    require(len(rows) == 7 and len({r["number"] for r in rows}) == 7
            and len({r["partuuid"] for r in rows}) == 7,
            "physical partition numbers or IDs are not unique")
    # A reboot can renumber Get-Partition rows by offset after insertion before
    # WinRE. The reviewed plan still records the original GPT entry slots.
    require(same_gpt_content(rows, plan["preserve_partitions"] + plan["target_partitions"]),
            "physical partition map differs from reviewed final map")
    current_by_id = {r["partuuid"]: r for r in rows}
    final_rows = []
    for row in plan["target_partitions"]:
        identity = stage.guid(row["partuuid"])
        final_rows.append(current_by_id[identity])
    return rows, final_rows
def read_gpt(stream, disk_size):
    """Read both GPT copies, including their CRCs, independently of Windows' cache."""
    require(disk_size % SECTOR == 0 and disk_size >= 68 * SECTOR, "invalid disk geometry")
    sectors = disk_size // SECTOR
    stream.seek(0)
    mbr = stream.read(SECTOR)
    require(len(mbr) == SECTOR and mbr[450] == 0xEE and mbr[510:] == b"\x55\xaa",
            "protective MBR missing")

    def header(lba):
        stream.seek(lba * SECTOR)
        raw = bytearray(stream.read(SECTOR))
        require(len(raw) == SECTOR, "short GPT header")
        fields = struct.unpack_from("<8sIIIIQQQQ16sQIII", raw)
        magic, revision, length, crc, reserved, current, backup, first, last, disk_id, table_lba, count, width, table_crc = fields
        require(magic == b"EFI PART" and revision == 0x10000 and 92 <= length <= SECTOR
                and reserved == 0 and current == lba and backup == (sectors - 1 if lba == 1 else 1)
                and 1 <= count <= 128 and width == 128, "invalid GPT header")
        struct.pack_into("<I", raw, 16, 0)
        require(zlib.crc32(raw[:length]) == crc, "GPT header CRC differs")
        table_bytes = count * width
        table_sectors = (table_bytes + SECTOR - 1) // SECTOR
        require(table_lba == (2 if lba == 1 else sectors - 1 - table_sectors)
                and 2 + table_sectors <= first <= last < sectors - 1 - table_sectors,
                "unexpected GPT table placement")
        stream.seek(table_lba * SECTOR)
        table = stream.read(table_bytes)
        require(len(table) == table_bytes and zlib.crc32(table) == table_crc, "GPT table CRC differs")
        return first, last, disk_id, count, width, table

    primary = header(1)
    require(primary == header(sectors - 1), "GPT copies disagree")
    first, last, disk_id, count, width, table = primary
    rows = []
    for index in range(count):
        entry = table[index * width:(index + 1) * width]
        if entry[:16] == bytes(16):
            require(entry == bytes(width), "unused GPT entry contains data")
            continue
        kind = str(uuid.UUID(bytes_le=entry[:16]))
        identity = str(uuid.UUID(bytes_le=entry[16:32]))
        start, end = struct.unpack_from("<QQ", entry, 32)
        require(first <= start <= end <= last, "partition exceeds GPT usable range")
        rows.append({"number": index + 1, "partuuid": identity, "type_guid": kind,
                     "offset_bytes": start * SECTOR, "size_bytes": (end - start + 1) * SECTOR})
    return str(uuid.UUID(bytes_le=disk_id)), normalized(rows)


def same_gpt_content(raw_rows, windows_rows):
    """Compare GPT identity and geometry; Windows numbers are offset ordinals.

    GPT entry slots can differ from Get-Partition numbers after a partition is
    deleted and its slot reused. Both maps still must contain distinct GUIDs.
    """
    raw = normalized(raw_rows)
    observed = normalized(windows_rows)
    if len(raw) != len(observed) or len({r["partuuid"] for r in raw}) != len(raw):
        return False
    if len({r["partuuid"] for r in observed}) != len(observed):
        return False
    keys = ("partuuid", "type_guid", "offset_bytes", "size_bytes")
    return [tuple(r[key] for key in keys) for r in raw] == [
        tuple(r[key] for key in keys) for r in observed]


def hash_range(stream, offset, size):
    stream.seek(offset)
    result = hashlib.sha256()
    left = size
    while left:
        data = stream.read(min(CHUNK, left))
        require(data and len(data) <= left, "short extent read")
        result.update(data)
        left -= len(data)
    return result.hexdigest()


def copy_range(source, target, row):
    offset, size = row["offset_bytes"], row["size_bytes"]
    source.seek(offset)
    target.seek(offset)
    result = hashlib.sha256()
    left = size
    while left:
        data = source.read(min(CHUNK, left))
        require(data and len(data) <= left, "source changed during copy")
        require(target.write(data) == len(data), "short physical write")
        result.update(data)
        left -= len(data)
    target.flush()
    os.fsync(target.fileno())
    return result.hexdigest()


def source_preflight(source_path, source_receipt, rows, final_rows, disk_size):
    require(not source_path.is_symlink() and source_path.is_file()
            and source_path.stat().st_size == disk_size,
            "VM-installed source raw is missing or changed in size")
    require(source_receipt.get("schema") == SCHEMA
            and source_receipt.get("raw_size_bytes") == disk_size,
            "VM-installed source receipt has wrong schema or size")
    source_rows = source_receipt.get("partitions")
    require(type(source_rows) is list and len(source_rows) == 7
            and len({r["number"] for r in normalized(source_rows)}) == 7
            and same_gpt_content(source_rows, rows),
            "VM-installed source receipt partition map differs")
    final = source_receipt.get("final_sha256")
    require(type(final) is dict and set(final) == set(ROLES), "VM-installed source lacks exact final hashes")
    for role in ROLES:
        digest(final[role])
    with source_path.open("rb", buffering=0) as source:
        _, actual_rows = read_gpt(source, disk_size)
        require(same_gpt_content(actual_rows, rows),
                "VM-installed source GPT differs from physical plan")
        for role, row in zip(ROLES, final_rows, strict=True):
            require(hash_range(source, row["offset_bytes"], row["size_bytes"]) == final[role],
                    f"VM-installed source {role} SHA-256 differs")
    return final


def validate_physical(plan, stage_receipt, current, target):
    rows, final_rows = expected_rows(plan, stage_receipt, current.get("partitions", []))
    require(stage.disk_matches(plan, current), "Windows disk identity or state changed")
    require(normalized(current.get("partitions", [])) == rows,
            "Windows partition map differs from reviewed final map")
    _, physical_rows = read_gpt(target, plan["windows_disk"]["size_bytes"])
    require(same_gpt_content(physical_rows, rows), "physical GPT differs from Windows partition map")
    return rows, final_rows


def transfer(source, target, rows, final_rows, expected_hashes, *, write,
             live_windows_partition_number):
    """File-backed test path and native executor share the same guarded copy."""
    final_ids = {r["partuuid"] for r in final_rows}
    preserved = [r for r in rows if r["partuuid"] not in final_ids]
    # C: stays mounted while Windows performs this transfer. Its contents may
    # legitimately change during a long copy, so a before/after whole-volume
    # hash cannot prove preservation. Its GPT extent is checked twice, and the
    # writer only seeks into the three reviewed final extents. All other
    # preserved partitions are checked byte-for-byte before and after.
    live = [r for r in preserved if r["number"] == live_windows_partition_number]
    require(len(live) == 1 and live[0]["type_guid"] == WINDOWS_DATA_GUID,
            "live Windows C: partition is not a preserved data partition")
    static_preserved = [r for r in preserved if r["number"] != live_windows_partition_number]
    before = {r["partuuid"]: hash_range(target, r["offset_bytes"], r["size_bytes"])
              for r in static_preserved}
    result = []
    for role, row in zip(ROLES, final_rows, strict=True):
        expected = expected_hashes[role]
        current_hash = hash_range(target, row["offset_bytes"], row["size_bytes"])
        if current_hash != expected:
            require(write, f"{role} is not installed")
            require(copy_range(source, target, row) == expected,
                    f"{role} source changed during copy")
        readback = hash_range(target, row["offset_bytes"], row["size_bytes"])
        require(readback == expected, f"{role} physical readback SHA-256 differs")
        result.append({"role": role, "partuuid": row["partuuid"], "sha256": readback,
                       "offset_bytes": row["offset_bytes"], "size_bytes": row["size_bytes"]})
    after = {r["partuuid"]: hash_range(target, r["offset_bytes"], r["size_bytes"])
             for r in static_preserved}
    require(after == before, "preserved Windows or staging partition changed")
    return result


def live_windows_partition(rows, disk_number):
    observed = stage.ps_json(r'''$ErrorActionPreference='Stop'
$p=Get-Partition -DriveLetter C -ErrorAction Stop
[pscustomobject]@{disk=[int]$p.DiskNumber;number=[int]$p.PartitionNumber;partuuid=([string]$p.Guid).Trim('{','}').ToLowerInvariant()}|ConvertTo-Json -Compress''')
    require(observed.get("disk") == disk_number, "C: is not on the reviewed disk")
    matches = [r for r in rows if r["number"] == observed.get("number")]
    require(len(matches) == 1 and matches[0]["partuuid"] == stage.guid(observed.get("partuuid"))
            and matches[0]["type_guid"] == WINDOWS_DATA_GUID,
            "C: identity differs from preserved Windows partition")
    return matches[0]["number"]


def run(args):
    plan = load_plan(args.plan)
    stage_receipt = None
    source_receipt = load_json(args.source_receipt)
    require(args.confirm_disk_id == plan["windows_disk"]["unique_id"],
            "confirmation disk ID differs from reviewed plan")
    require(platform.system() == "Windows" and ctypes.windll.shell32.IsUserAnAdmin(),
            "physical final copy requires elevated native Windows")
    if args.command == "execute":
        require(all(path is not None for path in
                    (args.ci_child_manifest, args.vm_proof_checked)),
                "execute requires CI manifests and checked VM install proof")
        require(re.fullmatch(r".+@sha256:[0-9a-f]{64}", args.ci_image or "") is not None
                and source_receipt.get("ci_image") == args.ci_image,
                "VM source receipt does not bind the digest-pinned CI image")
        if getattr(args, "development", False):
            ci_provenance, checked, dual_gate = development.verify(args)
            index_digest = child_digest = args.ci_image.split("@", 1)[1]
            require(source_receipt.get("ci_verification_key_sha256") is None,
                    "development source receipt must not claim a release verification key")
        else:
            key = args.verify_key
            require(key is not None and key.is_file() and not key.is_symlink()
                    and args.ci_index_manifest is not None
                    and source_receipt.get("ci_verification_key_sha256")
                    == hashlib.sha256(key.read_bytes()).hexdigest(),
                    "VM source receipt does not bind the CI verification key")
            index_digest, child_digest, children, kind = provenance.signed_ci_child(
                args.ci_image, args.ci_index_manifest, args.ci_child_manifest)
            require(kind == "lenovo-83ed",
                    "physical install requires the signed Lenovo ARM64 CI image")
            ci_provenance = provenance.verify_ci_image(
                args.ci_image, key, children, kind,
                args.cosign_command_json, args.cosign_key_arg)
            checked = provenance.checked_guest_proof(
                args.vm_proof_checked, child_digest, args.ci_image, ci_provenance)
            require(checked["schema"] == provenance.PROOF_SCHEMA_V2,
                    "physical install requires backend-derived signed target proof v2")
            dual_gate = provenance.dual_ci_medium_gate(
                args, checked, child_digest, ci_provenance["source_revision"])
        guest = checked["guest_proof"]
        require(source_receipt.get("ci_index_digest") == index_digest
                and source_receipt.get("ci_child_digest") == child_digest
                and source_receipt.get("ci_source_revision") == ci_provenance["source_revision"]
                and source_receipt.get("ci_invocation_id") == ci_provenance["invocation_id"]
                and source_receipt.get("ci_generation") == ci_provenance["generation"]
                and source_receipt.get("ci_image_kind") == ci_provenance["image_kind"]
                and source_receipt.get("ci_firmware_candidate") == ci_provenance["firmware_candidate"]
                and source_receipt.get("ci_firmware_manifest_sha256") == ci_provenance["firmware_manifest_sha256"]
                and source_receipt.get("ci_slsa_statement_sha256") == ci_provenance["statement_sha256"]
                and source_receipt.get("vm_proof_checked_sha256")
                == hashlib.sha256(args.vm_proof_checked.read_bytes()).hexdigest()
                and source_receipt.get("installed_uki_sha256") == guest["target_uki_sha256"],
                "VM source receipt does not bind verified CI child and checked guest proof")
        require(all(source_receipt.get(field) == value
                    for field, value in dual_gate.items()),
                "source receipt does not bind paired CI media and WHPX proofs")
    disk_number = plan["windows_disk"]["number"]
    current = stage.snapshot(disk_number)
    require(stage.disk_matches(plan, current), "Windows disk identity or state changed")
    # The caller must also have independently confirmed the matching BitLocker
    # recovery key before any physical write.
    if args.command == "execute":
        # Final copies cannot touch the Windows extent. Its pagefile is only a
        # shrink constraint; requiring it to disappear here forces a needless
        # Windows reboot after the final partitions have already been allocated.
        stage.windows_install_gate(require_no_pagefile=False)
    rows, final_rows = expected_rows(plan, stage_receipt, current.get("partitions", []))
    live_number = live_windows_partition(rows, disk_number)
    hashes = source_preflight(args.source_raw, source_receipt, rows, final_rows,
                              plan["windows_disk"]["size_bytes"])
    if args.command == "execute":
        with args.source_raw.open("rb", buffering=0) as source:
            source_disk_guid, _ = read_gpt(source, plan["windows_disk"]["size_bytes"])
            require(source_disk_guid == guest["disk_guid"],
                    "VM source disk identity differs from checked guest proof")
            require(provenance.installed_uki_sha256(
                source, final_rows[0], guest["composefs_digest"])
                == guest["target_uki_sha256"],
                "VM source ESP UKI differs from checked guest proof")
            loader_path, loader_hash = provenance.installed_loader(source, final_rows[0])
            require(source_receipt.get("final_esp_partuuid") == final_rows[0]["partuuid"]
                    and source_receipt.get("loader_path") == loader_path
                    and source_receipt.get("loader_sha256") == loader_hash,
                    "VM source ESP loader differs from source receipt")
    with stage.open_physical_drive(disk_number) as target:
        validate_physical(plan, stage_receipt, current, target)
        with args.source_raw.open("rb", buffering=0) as source:
            result = transfer(source, target, rows, final_rows, hashes,
                              write=args.command == "execute",
                              live_windows_partition_number=live_number)
        _, post_rows = read_gpt(target, plan["windows_disk"]["size_bytes"])
        require(same_gpt_content(post_rows, rows), "physical GPT changed during final copy")
    require(normalized(stage.snapshot(disk_number)["partitions"]) == rows,
            "Windows partition map changed during final copy")
    output = {"schema": "tilo.windows-direct-final-readback.v1", "disk": plan["windows_disk"],
              "final": result,
              "static_preserved_partitions_sha256_compared": len(rows) - len(final_rows) - 1,
              "live_windows_partition": live_number,
              "live_windows_content_hash_compared": False,
              "physical_readback_verified": True}
    if args.command == "execute":
        output["ci_index_digest"] = index_digest
        output["ci_child_digest"] = child_digest
        output["installed_uki_sha256"] = guest["target_uki_sha256"]
        output["vm_proof_checked_sha256"] = source_receipt["vm_proof_checked_sha256"]
        output["final_esp_partuuid"] = final_rows[0]["partuuid"]
        output["loader_path"] = loader_path
        output["loader_sha256"] = loader_hash
        if getattr(args, "development", False):
            output["development_admission"] = dual_gate["development_admission"]
        require(args.output is not None, "execute requires --output")
        stage.atomic_json(args.output, output)
    print(json.dumps(output, indent=2))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("verify", "execute"):
        command = commands.add_parser(name)
        command.add_argument("--plan", type=Path, required=True)
        command.add_argument("--source-raw", type=Path, required=True)
        command.add_argument("--source-receipt", type=Path, required=True)
        command.add_argument("--confirm-disk-id", required=True)
        command.add_argument("--ci-image", help="digest-pinned CI-signed OCI image; required for execute")
        command.add_argument("--verify-key", type=Path,
                             help="CI Cosign public key; required for execute")
        command.add_argument("--cosign-command-json", type=Path)
        command.add_argument("--cosign-key-arg")
        command.add_argument("--ci-index-manifest", type=Path)
        command.add_argument("--ci-child-manifest", type=Path)
        command.add_argument("--vm-proof-checked", type=Path)
        development.add_arguments(command)
        for field in ("generic-ci-image", "generic-index-manifest",
                      "generic-child-manifest", "generic-vm-proof-checked",
                      "generic-medium-identity", "generic-ci-guest-proof",
                      "board-medium-identity", "board-ci-guest-proof"):
            command.add_argument("--" + field, type=str if field == "generic-ci-image" else Path)
        command.add_argument("--output", type=Path)
        command.set_defaults(handler=run)
    args = parser.parse_args(argv)
    try:
        args.handler(args)
        return 0
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError,
            subprocess.TimeoutExpired) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
