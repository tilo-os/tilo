"""Validated, declarative hardware definitions shared by image and UKI builders."""

import json
from pathlib import Path
import re
import stat
import sys


PROFILES_DIR = Path(__file__).resolve().parents[1] / "hardware/profiles"
DEFINITION_PATH = Path("usr/lib/qos/hardware-profile-definition.json")
ARCH_CONFIG = {"x86_64": "CONFIG_X86_64=y", "aarch64": "CONFIG_ARM64=y"}
FIELDS = {"schema_version", "id", "architecture", "devicetree", "firmware"}
PROFILE_PATH = Path("usr/lib/qos/hardware-profile.json")
PROFILE_FIELDS = {"schema_version", "id", "architecture", "devicetree", "devicetree_sha256", "target_update_ref"}
DEFAULT_TARGET_IMGREF = "code.sandbox.example/qos/qos:latest"


def validate_target_imgref(value):
    """Only this exact repository is signature-required by the image policy."""
    if not isinstance(value, str) or not re.fullmatch(
        r"code\.podesta\.ai/qos/qos(?::[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}|@sha256:[0-9a-f]{64})", value
    ):
        raise ValueError("QOS_TARGET_IMGREF must explicitly name a valid tag or sha256 digest in the signature-required code.sandbox.example/qos/qos repository")
    return value


def trusted_path(root, relative, *, regular):
    """Reject writable, non-root-owned or redirected immutable image inputs."""
    current = root
    for index, part in enumerate(("", *relative.parts)):
        if part:
            current = current / part
        info = current.lstat()
        if stat.S_ISLNK(info.st_mode):
            raise ValueError(f"hardware profile input contains a symlink: {current}")
        if info.st_uid != 0 or info.st_mode & 0o022:
            raise ValueError(f"hardware profile input must be root-owned and not group/world writable: {current}")
        if regular and index == len(relative.parts):
            if not stat.S_ISREG(info.st_mode):
                raise ValueError(f"hardware profile input is not a regular file: {current}")
        elif not stat.S_ISDIR(info.st_mode):
            raise ValueError(f"hardware profile input is not a directory: {current}")
    return current


def read_profile(root):
    candidate = root / PROFILE_PATH
    if not candidate.exists() and not candidate.is_symlink():
        definition = root / DEFINITION_PATH
        if definition.exists() or definition.is_symlink():
            raise ValueError("hardware definition has no generated hardware profile")
        return None
    profile = trusted_path(root, PROFILE_PATH, regular=True)
    if profile.stat().st_size > 16384:
        raise ValueError("hardware profile is too large")
    data = json.loads(profile.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or set(data) != PROFILE_FIELDS:
        raise ValueError("hardware profile has missing or unknown fields")
    if type(data["schema_version"]) is not int or data["schema_version"] != 1:
        raise ValueError("unsupported hardware profile schema")
    definition = read_definition(trusted_path(root, DEFINITION_PATH, regular=True))
    if data["id"] != definition["id"]:
        raise ValueError("hardware profile id does not match its image-owned definition")
    if data["architecture"] != definition["architecture"]:
        raise ValueError("hardware profile requires a matching native image architecture")
    validate_target_imgref(data["target_update_ref"])
    return data


def install_update_origin(root, override=""):
    data = read_profile(root)
    if data is None:
        return validate_target_imgref(override or DEFAULT_TARGET_IMGREF)
    bound = data["target_update_ref"]
    if override and override != bound:
        raise ValueError("QOS_TARGET_IMGREF conflicts with the image-owned hardware profile update reference")
    return bound


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
    try:
        if len(sys.argv) == 3 and sys.argv[1] == "--install-update-origin":
            # Runtime callers cannot redirect metadata to a writable path.
            print(install_update_origin(Path("/"), sys.argv[2]))
            return
        if len(sys.argv) != 6:
            raise ValueError("usage: hardware_profile.py PROFILE DEFINITIONS ARCHITECTURE FIRMWARE_ROOT TARGET_UPDATE_REF")
        name, directory, architecture, firmware_root, target_update_ref = sys.argv[1:]
        definition = load_definition(name, Path(directory))
        if architecture != definition["architecture"]:
            raise ValueError(f"{name} requires a native {definition['architecture']} builder")
        if definition["firmware"]:
            manifest = Path(firmware_root) / name / "manifest.json"
            if not manifest.is_file() or manifest.stat().st_size == 0:
                raise ValueError(f"stage the {name} firmware manifest first")
        validate_target_imgref(target_update_ref)
    except (OSError, ValueError, TypeError) as error:
        raise SystemExit(f"hardware profile rejected: {error}") from error


if __name__ == "__main__":
    main()
