"""Validated, declarative hardware definitions shared by image and UKI builders."""

import json
from pathlib import Path
import re
import sys


PROFILES_DIR = Path(__file__).resolve().parents[1] / "hardware/profiles"
DEFINITION_PATH = Path("usr/lib/qos/hardware-profile-definition.json")
ARCH_CONFIG = {"x86_64": "CONFIG_X86_64=y", "aarch64": "CONFIG_ARM64=y"}
FIELDS = {"schema_version", "id", "architecture", "devicetree", "firmware"}


def profile_id(value):
    if not isinstance(value, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", value):
        raise ValueError("invalid hardware profile id")
    return value


def relative_path(value):
    if not isinstance(value, str) or not value or any(
        part in ("", ".", "..") or not re.fullmatch(r"[A-Za-z0-9_.+-]+", part)
        for part in value.split("/")
    ):
        raise ValueError("hardware definition path must be a safe relative path")
    return Path(value)


def validate_definition(data):
    if not isinstance(data, dict) or set(data) != FIELDS:
        raise ValueError("hardware definition has missing or unknown fields")
    if type(data["schema_version"]) is not int or data["schema_version"] != 1:
        raise ValueError("unsupported hardware definition schema")
    profile_id(data["id"])
    if data["id"] == "generic":
        raise ValueError("generic is reserved for the standard firmware boot path")
    if not isinstance(data["architecture"], str) or data["architecture"] not in ARCH_CONFIG:
        raise ValueError("unsupported hardware definition architecture")
    if data["devicetree"] is not None:
        dtb = relative_path(data["devicetree"])
        if data["architecture"] != "aarch64" or dtb.suffix != ".dtb":
            raise ValueError("device-tree profiles require an aarch64 DTB")
    firmware = data["firmware"]
    if not isinstance(firmware, dict) or len(firmware) > 256:
        raise ValueError("hardware definition firmware must be a bounded file mapping")
    destinations = set()
    for source, destination in firmware.items():
        if len(relative_path(source).parts) != 1:
            raise ValueError("hardware firmware source must be a basename")
        relative_path(destination)
        if any(destination == other or destination.startswith(other + "/") or other.startswith(destination + "/")
               for other in destinations):
            raise ValueError("hardware firmware destinations must be unique and must not overlap")
        destinations.add(destination)
    return data


def read_definition(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 65536:
        raise ValueError("hardware definition must be a regular bounded file")
    return validate_definition(json.loads(path.read_text(encoding="utf-8")))


def load_definition(name, directory=PROFILES_DIR):
    profile_id(name)
    path = directory / f"{name}.json"
    if not path.exists():
        raise ValueError(f"unsupported hardware profile: {name}")
    if directory.is_symlink():
        raise ValueError("hardware definition directory must not be a symlink")
    definition = read_definition(path)
    if definition["id"] != name:
        raise ValueError("hardware definition id does not match its filename")
    return definition


def main():
    if len(sys.argv) != 5:
        raise SystemExit("usage: hardware_profile.py PROFILE DEFINITIONS ARCHITECTURE FIRMWARE_ROOT")
    name, directory, architecture, firmware_root = sys.argv[1:]
    try:
        definition = load_definition(name, Path(directory))
        if architecture != definition["architecture"]:
            raise ValueError(f"{name} requires a native {definition['architecture']} builder")
        if definition["firmware"]:
            manifest = Path(firmware_root) / name / "manifest.json"
            if not manifest.is_file() or manifest.stat().st_size == 0:
                raise ValueError(f"stage the {name} firmware manifest first")
    except (OSError, ValueError, TypeError) as error:
        raise SystemExit(f"hardware profile rejected: {error}") from error


if __name__ == "__main__":
    main()
