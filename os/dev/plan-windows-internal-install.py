#!/usr/bin/env python3
"""Plan a USB-free Tilo install from a verified raw installer image.

This command is deliberately read-only.  It binds a source receipt, the raw
image's GPT, a Windows disk snapshot, and one exact unallocated extent into a
machine-readable operation plan.  A separate elevated executor can then create
only the two staging partitions described by that plan.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import struct
import sys
import tempfile
import uuid
import zlib

SECTOR = 512
MIB = 1024 * 1024
GIB = 1024 * MIB
ESP_GUID = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
TILO_ROOT_GUID = "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab"
MIN_FINAL_ROOT = 32 * GIB
PLAN_SCHEMA = "tilo.windows-internal-install-plan.v1"


def fail(message: str) -> None:
    raise ValueError(message)


def exact(stream, offset: int, size: int) -> bytes:
    stream.seek(offset)
    value = stream.read(size)
    if len(value) != size:
        fail("installer image is truncated")
    return value


def read_json(path: Path) -> dict:
    if path.is_symlink():
        fail(f"refusing symlink: {path}")
    value = json.loads(path.read_bytes())
    if type(value) is not dict:
        fail(f"expected a JSON object: {path}")
    return value


def canonical_guid(value: object) -> str:
    try:
        return str(uuid.UUID(str(value).strip().strip("{}")))
    except (ValueError, AttributeError) as error:
        raise ValueError(f"invalid GUID: {value!r}") from error


def checked_int(value: object, label: str, minimum: int = 0) -> int:
    if type(value) is not int or value < minimum:
        fail(f"{label} is invalid")
    return value


def parse_gpt(image: Path) -> tuple[str, list[dict]]:
    if image.is_symlink() or not image.is_file():
        fail("installer image must be a regular non-symlink file")
    size = image.stat().st_size
    if size < 68 * SECTOR or size % SECTOR:
        fail("installer image has invalid sector geometry")
    sectors = size // SECTOR
    with image.open("rb", buffering=0) as stream:
        mbr = exact(stream, 0, SECTOR)
        if (mbr[510:] != b"\x55\xaa" or mbr[446] != 0 or mbr[450] != 0xEE
                or struct.unpack_from("<II", mbr, 454) != (1, min(sectors - 1, 0xFFFFFFFF))
                or mbr[462:510] != bytes(48)):
            fail("installer image lacks a protective MBR")

        def header(lba: int):
            raw = bytearray(exact(stream, lba * SECTOR, SECTOR))
            fields = struct.unpack_from("<8sIIIIQQQQ16sQIII", raw)
            signature, revision, length, crc, reserved, current, backup, first, last, disk_id, table_lba, count, width, table_crc = fields
            if signature != b"EFI PART" or revision != 0x10000 or length != 92 or reserved:
                fail("unsupported GPT header")
            struct.pack_into("<I", raw, 16, 0)
            if zlib.crc32(raw[:length]) != crc:
                fail("GPT header checksum mismatch")
            expected_backup = sectors - 1 if lba == 1 else 1
            if current != lba or backup != expected_backup or not (1 <= count <= 128) or width != 128:
                fail("unsupported GPT geometry")
            table_size = count * width
            table_sectors = (table_size + SECTOR - 1) // SECTOR
            expected_table = 2 if lba == 1 else sectors - 1 - table_sectors
            if table_lba != expected_table or not 2 + table_sectors <= first <= last < sectors - 1 - table_sectors:
                fail("noncanonical GPT metadata placement")
            table = exact(stream, table_lba * SECTOR, table_size)
            if zlib.crc32(table) != table_crc:
                fail("GPT entry checksum mismatch")
            return first, last, disk_id, count, width, table

        primary = header(1)
        backup = header(sectors - 1)
        if primary != backup:
            fail("primary and backup GPT disagree")
        first, last, disk_id, count, width, table = primary
        partitions = []
        for index in range(count):
            row = table[index * width:(index + 1) * width]
            if row[:16] == bytes(16):
                if row != bytes(width):
                    fail("unused GPT entry contains data")
                continue
            kind = str(uuid.UUID(bytes_le=row[:16]))
            identity = str(uuid.UUID(bytes_le=row[16:32]))
            start, end, attributes = struct.unpack_from("<QQQ", row, 32)
            if not first <= start <= end <= last or attributes != 0:
                fail("unsupported source partition geometry or attributes")
            partitions.append({
                "number": index + 1,
                "type_guid": kind,
                "partuuid": identity,
                "offset_bytes": start * SECTOR,
                "size_bytes": (end - start + 1) * SECTOR,
            })
        return str(uuid.UUID(bytes_le=disk_id)), partitions


def normalize_snapshot(value: dict) -> tuple[dict, list[dict]]:
    disk = value.get("Disk") or value.get("disk")
    rows = value.get("Partitions") or value.get("partitions")
    if type(disk) is not dict or type(rows) is not list:
        fail("Windows snapshot lacks Disk/Partitions")
    normalized_disk = {
        "number": checked_int(disk.get("Number", disk.get("number")), "disk number"),
        "unique_id": str(disk.get("UniqueId", disk.get("unique_id", ""))).strip(),
        "serial": str(disk.get("SerialNumber", disk.get("serial", ""))).strip(),
        "size_bytes": checked_int(disk.get("Size", disk.get("size_bytes")), "disk size", 1),
        "logical_sector_size": checked_int(disk.get("LogicalSectorSize", disk.get("logical_sector_size")), "logical sector size", 1),
        "partition_style": str(disk.get("PartitionStyle", disk.get("partition_style", ""))).upper(),
        "is_boot": bool(disk.get("IsBoot", disk.get("is_boot"))),
        "is_system": bool(disk.get("IsSystem", disk.get("is_system"))),
        "is_offline": bool(disk.get("IsOffline", disk.get("is_offline"))),
        "is_read_only": bool(disk.get("IsReadOnly", disk.get("is_read_only"))),
    }
    if not normalized_disk["unique_id"] or not normalized_disk["serial"]:
        fail("Windows disk snapshot lacks durable identity")
    if normalized_disk["partition_style"] != "GPT" or normalized_disk["logical_sector_size"] != SECTOR:
        fail("only 512-byte-sector GPT Windows disks are supported")
    if not normalized_disk["is_boot"] or not normalized_disk["is_system"] or normalized_disk["is_offline"] or normalized_disk["is_read_only"]:
        fail("snapshot is not the online writable Windows system disk")
    partitions = []
    for row in rows:
        if type(row) is not dict:
            fail("invalid Windows partition row")
        partitions.append({
            "number": checked_int(row.get("PartitionNumber", row.get("number")), "partition number", 1),
            "partuuid": canonical_guid(row.get("Guid", row.get("partuuid"))),
            "type_guid": canonical_guid(row.get("GptType", row.get("type_guid"))),
            "offset_bytes": checked_int(row.get("Offset", row.get("offset_bytes")), "partition offset", 1),
            "size_bytes": checked_int(row.get("Size", row.get("size_bytes")), "partition size", 1),
        })
    partitions.sort(key=lambda row: row["offset_bytes"])
    previous = MIB
    identities = set()
    for row in partitions:
        if row["offset_bytes"] < previous or row["offset_bytes"] + row["size_bytes"] > normalized_disk["size_bytes"]:
            fail("Windows partitions overlap or exceed the disk")
        if row["partuuid"] in identities:
            fail("Windows partition identities are not unique")
        identities.add(row["partuuid"])
        previous = row["offset_bytes"] + row["size_bytes"]
    return normalized_disk, partitions


def source_from_receipt(image: Path, receipt: dict) -> tuple[dict, dict]:
    artifact = receipt.get("artifact")
    hardware = receipt.get("hardware")
    if receipt.get("schema") != 1 or type(artifact) is not dict or type(hardware) is not dict:
        fail("unsupported installer receipt")
    if hardware.get("architecture") != "aarch64" or not re.fullmatch(r"[a-z0-9-]{1,64}", str(hardware.get("profile", ""))):
        fail("receipt is not a bounded ARM64 hardware profile")
    expected_size = checked_int(artifact.get("bytes"), "receipt artifact size", 1)
    expected_hash = str(artifact.get("sha256", ""))
    if image.stat().st_size != expected_size or not re.fullmatch(r"[0-9a-f]{64}", expected_hash):
        fail("installer image size or receipt digest is invalid")
    disk_id, partitions = parse_gpt(image)
    if len(partitions) != 2:
        fail("installer image must contain exactly an ESP and Tilo root")
    by_type = {row["type_guid"]: row for row in partitions}
    if set(by_type) != {ESP_GUID, TILO_ROOT_GUID}:
        fail("installer image partition types do not match the Tilo media contract")
    return {
        "path": str(image.resolve()),
        "size_bytes": expected_size,
        "sha256": expected_hash,
        "disk_guid": disk_id,
        "profile": hardware["profile"],
    }, {"staging_esp": by_type[ESP_GUID], "staging_root": by_type[TILO_ROOT_GUID]}


def ranges_overlap(left_start: int, left_size: int, right_start: int, right_size: int) -> bool:
    return left_start < right_start + right_size and right_start < left_start + left_size


def build_plan(image: Path, receipt: dict, snapshot: dict, free_start: int, free_size: int,
               shrink_partition_number: int | None = None,
               shrink_size_bytes: int | None = None) -> dict:
    source, components = source_from_receipt(image, receipt)
    disk, existing = normalize_snapshot(snapshot)
    free_end = free_start + free_size
    if free_start % MIB or free_size % MIB or free_start < MIB or free_end > disk["size_bytes"]:
        fail("requested free extent is unaligned or outside the disk")
    planned_existing = [dict(row) for row in existing]
    if (shrink_partition_number is None) != (shrink_size_bytes is None):
        fail("shrink partition number and size must be supplied together")
    if shrink_partition_number is not None:
        matches = [row for row in planned_existing if row["number"] == shrink_partition_number]
        if len(matches) != 1:
            fail("planned shrink partition is missing or ambiguous")
        shrink = matches[0]
        if not (0 < shrink_size_bytes < shrink["size_bytes"]):
            fail("planned shrink must strictly reduce the selected partition")
        shrink["size_bytes"] = shrink_size_bytes
        if shrink["offset_bytes"] + shrink["size_bytes"] != free_start:
            fail("planned shrink does not end at the reviewed free extent")
    for row in planned_existing:
        if ranges_overlap(free_start, free_size, row["offset_bytes"], row["size_bytes"]):
            fail("requested free extent overlaps an existing Windows partition")
    esp = components["staging_esp"]
    root = components["staging_root"]
    staging_esp_start = free_end - esp["size_bytes"]
    staging_root_start = staging_esp_start - root["size_bytes"]
    target_free_size = staging_root_start - free_start
    if min(staging_root_start, staging_esp_start) % MIB or target_free_size < 2 * GIB + MIN_FINAL_ROOT:
        fail("free extent cannot hold the exact media plus a viable final Tilo installation")
    return {
        "schema": PLAN_SCHEMA,
        "read_only_plan": True,
        "source": source,
        "windows_disk": disk,
        "observed_partitions": existing,
        "preserve_partitions": planned_existing,
        "planned_windows_shrink": None if shrink_partition_number is None else {
            "partition_number": shrink_partition_number,
            "original_size_bytes": next(row["size_bytes"] for row in existing if row["number"] == shrink_partition_number),
            "new_size_bytes": shrink_size_bytes,
        },
        "reviewed_extent": {"offset_bytes": free_start, "size_bytes": free_size},
        "target_free_extent": {"offset_bytes": free_start, "size_bytes": target_free_size},
        "target_partitions": [
            {"role": "final-esp", "partuuid": str(uuid.uuid4()), "type_guid": ESP_GUID,
             "offset_bytes": free_start, "size_bytes": 2 * GIB},
            {"role": "final-root", "partuuid": str(uuid.uuid4()), "type_guid": TILO_ROOT_GUID,
             "offset_bytes": free_start + 2 * GIB, "size_bytes": target_free_size - 2 * GIB},
        ],
        "staging": [
            {"role": "staging-root", "type_guid": TILO_ROOT_GUID,
             "offset_bytes": staging_root_start, "size_bytes": root["size_bytes"],
             "source_offset_bytes": root["offset_bytes"], "source_partuuid": root["partuuid"]},
            {"role": "staging-esp", "type_guid": ESP_GUID,
             "offset_bytes": staging_esp_start, "size_bytes": esp["size_bytes"],
             "source_offset_bytes": esp["offset_bytes"], "source_partuuid": esp["partuuid"]},
        ],
        "post_boot_reclaim": {
            "delete_roles_after_verified_tilo_boot": ["staging-root", "staging-esp"],
            "grow_final_root_into_reclaimed_extent": True,
        },
    }


def atomic_json(path: Path, value: dict) -> None:
    path = path.resolve(strict=False)
    if not path.parent.is_dir():
        fail(f"output directory does not exist: {path.parent}")
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write((json.dumps(value, indent=2, sort_keys=True) + "\n").encode())
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def positive(value: str) -> int:
    try:
        parsed = int(value, 10)
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected a base-10 integer") from error
    if parsed <= 0:
        raise argparse.ArgumentTypeError("expected a positive integer")
    return parsed


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--windows-snapshot", type=Path, required=True)
    parser.add_argument("--free-start-bytes", type=positive, required=True)
    parser.add_argument("--free-size-bytes", type=positive, required=True)
    parser.add_argument("--shrink-partition-number", type=positive)
    parser.add_argument("--shrink-size-bytes", type=positive)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        plan = build_plan(args.image, read_json(args.receipt), read_json(args.windows_snapshot),
                          args.free_start_bytes, args.free_size_bytes,
                          args.shrink_partition_number, args.shrink_size_bytes)
        atomic_json(args.output, plan)
        print(f"wrote read-only internal-install plan: {args.output}")
        return 0
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
