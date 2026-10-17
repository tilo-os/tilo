#!/usr/bin/env python3
"""Read-only Windows gate for a Tilo alongside-installation session.

It runs before booting the Linux installer, which cannot authoritatively inspect
Windows BitLocker or the current unallocated extent. It never changes disks,
BitLocker, firmware, paging, or Windows configuration.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from uuid import UUID

SCHEMA = "tilo.windows-dual-boot-preflight.v1"
EFI_SYSTEM_GUID = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
REMOVABLE_BUSES = {"USB", "SD", "MMC"}


def fail(message):
    raise ValueError(message)


def one_or_many(value):
    return [] if value is None else value if isinstance(value, list) else [value]


def require_windows():
    if platform.system() != "Windows":
        fail("Windows dual-boot preflight must run in native Windows, not WSL")


def powershell(script):
    result = subprocess.run(
        ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script],
        capture_output=True, text=True, timeout=120,
    )
    if result.returncode:
        fail(f"PowerShell failed (exit {result.returncode}): {(result.stderr or result.stdout).strip()}")
    return result.stdout


def integer(value):
    try:
        parsed = int(value, 10)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be a base-10 integer") from error
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def canonical_guid(value):
    return str(value or "").strip().strip("{}").lower()


def normalized_disk(row):
    return {
        "number": int(row.get("Number", -1)),
        "friendly_name": " ".join(str(row.get("FriendlyName") or "").split()),
        "unique_id": str(row.get("UniqueId") or "").strip(),
        "size_bytes": int(row.get("Size", -1)),
        "bus_type": str(row.get("BusType") or "").upper(),
        # Unknown or malformed facts must not become an affirmative safety check.
        "is_boot": row.get("IsBoot"), "is_system": row.get("IsSystem"),
        "is_offline": row.get("IsOffline"), "is_read_only": row.get("IsReadOnly"),
        "largest_free_extent_bytes": int(row.get("LargestFreeExtent", -1)),
        "partition_style": str(row.get("PartitionStyle") or "").upper(),
    }


def normalized_partition(row):
    return {
        "number": int(row.get("PartitionNumber", -1)),
        "guid": canonical_guid(row.get("Guid")),
        "gpt_type": canonical_guid(row.get("GptType")),
        "offset_bytes": int(row.get("Offset", -1)), "size_bytes": int(row.get("Size", -1)),
        "type": str(row.get("Type") or ""),
    }


def make_receipt(observed, windows_disk_number, media_disk_number):
    windows = normalized_disk(observed.get("WindowsDisk") or {})
    media = normalized_disk(observed.get("MediaDisk") or {})
    if windows["number"] != windows_disk_number or media["number"] != media_disk_number:
        fail("disk observation did not match the requested disk number")
    partitions = [normalized_partition(partition) for partition in one_or_many(observed.get("Partitions"))]
    esps = [partition for partition in partitions if partition["gpt_type"] == EFI_SYSTEM_GUID]
    if len(esps) != 1:
        fail("Windows disk must expose exactly one EFI System Partition")
    bitlocker = observed.get("BitLocker") or {}
    return {
        "schema": SCHEMA, "observed_at_utc": datetime.now(timezone.utc).isoformat(), "read_only": True,
        "windows_disk": windows, "windows_esp": esps[0], "external_media": media,
        "bitlocker": {
            "mount_point": str(bitlocker.get("MountPoint") or ""),
            "volume_status": str(bitlocker.get("VolumeStatus") or ""),
            "protection_status": str(bitlocker.get("ProtectionStatus") or ""),
            "encryption_percentage": bitlocker.get("EncryptionPercentage"),
            "lock_status": str(bitlocker.get("LockStatus") or ""),
        },
    }


def collect_snapshot(windows_disk_number, media_disk_number):
    require_windows()
    if windows_disk_number == media_disk_number:
        fail("Windows and removable-media disk numbers must differ")
    script = rf'''$ErrorActionPreference='Stop'
$windowsDisk = Get-Disk -Number {windows_disk_number} -ErrorAction Stop
$mediaDisk = Get-Disk -Number {media_disk_number} -ErrorAction Stop
$partitions = @(Get-Partition -DiskNumber {windows_disk_number} -ErrorAction Stop | ForEach-Object {{
  [PSCustomObject]@{{PartitionNumber=[int]$_.PartitionNumber; Guid=[string]$_.Guid; GptType=[string]$_.GptType; Offset=[long]$_.Offset; Size=[long]$_.Size; Type=[string]$_.Type}}
}})
$bitlocker = Get-BitLockerVolume -MountPoint C: -ErrorAction Stop | Select-Object MountPoint,VolumeStatus,ProtectionStatus,EncryptionPercentage,LockStatus
function DiskRow($d) {{ [PSCustomObject]@{{Number=[int]$d.Number; FriendlyName=[string]$d.FriendlyName; UniqueId=[string]$d.UniqueId; Size=[long]$d.Size; BusType=[string]$d.BusType; IsBoot=[bool]$d.IsBoot; IsSystem=[bool]$d.IsSystem; IsOffline=[bool]$d.IsOffline; IsReadOnly=[bool]$d.IsReadOnly; LargestFreeExtent=[long]$d.LargestFreeExtent; PartitionStyle=[string]$d.PartitionStyle}} }}
[PSCustomObject]@{{WindowsDisk=(DiskRow $windowsDisk); MediaDisk=(DiskRow $mediaDisk); Partitions=$partitions; BitLocker=$bitlocker}} | ConvertTo-Json -Depth 5 -Compress'''
    try:
        observed = json.loads(powershell(script))
    except json.JSONDecodeError as error:
        fail(f"PowerShell returned invalid JSON: {error}")
    return make_receipt(observed, windows_disk_number, media_disk_number)


def load_receipt(path):
    if path.is_symlink():
        fail("preflight receipt must not be a symlink")
    try:
        receipt = json.loads(path.read_bytes())
    except FileNotFoundError as error:
        raise ValueError(f"preflight receipt is missing: {path}") from error
    except json.JSONDecodeError as error:
        raise ValueError(f"preflight receipt is invalid JSON: {error}") from error
    if not isinstance(receipt, dict) or receipt.get("schema") != SCHEMA or receipt.get("read_only") is not True:
        fail("preflight receipt has the wrong schema or is not read-only")
    if not all(isinstance(receipt.get(key), dict) for key in ("windows_disk", "windows_esp", "bitlocker", "external_media")):
        fail("preflight receipt lacks required facts")
    return receipt


def bitlocker_ready_for_planned_boot_change(bitlocker):
    """Accept a fully decrypted volume or an encrypted, explicitly suspended one.

    Tilo never writes the Windows volume or its ESP.  Decrypting C: just to
    create adjacent Tilo partitions is therefore needless exposure and a very
    slow recovery path.  Windows exposes a bounded BitLocker suspension as
    ``ProtectionStatus: Off`` while preserving full encryption.  Rejecting
    partially encrypted/decrypted and locked volumes keeps this receipt useful
    for the short installation/boot-chain-change window.
    """
    volume = str(bitlocker.get("volume_status") or "").strip().casefold()
    protection = str(bitlocker.get("protection_status") or "").strip().casefold()
    lock_status = str(bitlocker.get("lock_status") or "").strip().casefold()
    percentage = bitlocker.get("encryption_percentage")
    if protection not in {"off", "0"} or lock_status not in {"unlocked", "0"}:
        return False
    if volume in {"fullyencrypted", "1"}:
        return percentage in {100, "100", 100.0}
    if volume in {"fullydecrypted", "0"}:
        return percentage in {0, "0", 0.0}
    return False


def has_windows_esp_identity(receipt):
    disk, esp = receipt["windows_disk"], receipt["windows_esp"]
    if (type(disk.get("number")) is not int or disk["number"] < 0
            or not isinstance(disk.get("unique_id"), str) or not disk["unique_id"].strip()
            or type(disk.get("size_bytes")) is not int or disk["size_bytes"] <= 0
            or disk.get("partition_style") != "GPT"
            or type(esp.get("number")) is not int or esp["number"] <= 0
            or type(esp.get("offset_bytes")) is not int or esp["offset_bytes"] < 0
            or type(esp.get("size_bytes")) is not int or esp["size_bytes"] <= 0
            or esp.get("gpt_type") != EFI_SYSTEM_GUID):
        return False
    try:
        return isinstance(esp.get("guid"), str) and UUID(esp["guid"]).int != 0
    except ValueError:
        return False


def same_windows_esp(before, current):
    # Equality of absent/default identities is not evidence of an unchanged ESP.
    if not has_windows_esp_identity(before) or not has_windows_esp_identity(current):
        return False
    disk_keys = ("number", "unique_id", "size_bytes", "partition_style")
    esp_keys = ("number", "guid", "gpt_type", "offset_bytes", "size_bytes")
    return all(before["windows_disk"].get(key) == current["windows_disk"].get(key) for key in disk_keys) and all(before["windows_esp"].get(key) == current["windows_esp"].get(key) for key in esp_keys)


def valid_external_media(media):
    return (
        media.get("bus_type") in REMOVABLE_BUSES and isinstance(media.get("number"), int)
        and bool(media.get("unique_id")) and bool(media.get("friendly_name"))
        and int(media.get("size_bytes", -1)) > 0 and media.get("is_boot") is False
        and media.get("is_system") is False and media.get("is_read_only") is False
        and media.get("is_offline") is False
    )


def verify_receipt(current, baseline, required_unallocated_bytes):
    failures = []
    if not bitlocker_ready_for_planned_boot_change(current["bitlocker"]):
        failures.append("BitLocker on C: must be fully encrypted with protection suspended, or fully decrypted, and unlocked")
    free_extent = int(current["windows_disk"].get("largest_free_extent_bytes", -1))
    if free_extent < required_unallocated_bytes:
        failures.append(f"largest Windows-disk unallocated extent is {free_extent} bytes; need at least {required_unallocated_bytes}")
    if not valid_external_media(current["external_media"]):
        failures.append("an identified, writable external USB/SD/MMC medium is required")
    if current["external_media"].get("number") == current["windows_disk"].get("number"):
        failures.append("external medium must not be the Windows disk")
    if not same_windows_esp(baseline, current):
        failures.append("Windows disk or EFI System Partition identity is missing, invalid, or changed from the baseline")
    return failures


def atomic_json(path, value):
    path = path.resolve(strict=False)
    if not path.parent.is_dir():
        fail(f"output directory does not exist: {path.parent}")
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write((json.dumps(value, indent=2, sort_keys=True) + "\n").encode())
            output.flush(); os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        try: os.unlink(temporary)
        except FileNotFoundError: pass


def collect(args):
    atomic_json(args.output, collect_snapshot(args.windows_disk_number, args.media_disk_number))
    print(f"wrote read-only preflight receipt: {args.output}")


def verify(args):
    failures = verify_receipt(load_receipt(args.receipt), load_receipt(args.baseline), args.required_unallocated_bytes)
    if failures:
        fail("alongside preflight blocked:\n- " + "\n- ".join(failures))
    print("alongside preflight passed")


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)
    collect_parser = commands.add_parser("collect", help="collect a read-only elevated Windows snapshot")
    collect_parser.add_argument("--windows-disk-number", type=integer, required=True)
    collect_parser.add_argument("--media-disk-number", type=integer, required=True)
    collect_parser.add_argument("--output", type=Path, required=True)
    collect_parser.set_defaults(handler=collect)
    verify_parser = commands.add_parser("verify", help="block until a snapshot meets alongside prerequisites")
    verify_parser.add_argument("--receipt", type=Path, required=True)
    verify_parser.add_argument("--baseline", type=Path, required=True)
    verify_parser.add_argument("--required-unallocated-bytes", type=integer, required=True)
    verify_parser.set_defaults(handler=verify)
    return result


def main(argv=None):
    try:
        args = parser().parse_args(argv); args.handler(args); return 0
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(f"error: {error}", file=sys.stderr); return 1


if __name__ == "__main__":
    raise SystemExit(main())
