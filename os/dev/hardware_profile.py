"""Validated, declarative hardware definitions shared by image and UKI builders."""

import hashlib
import json
from pathlib import Path
import re
import stat
import sys


PROFILES_DIR = Path(__file__).resolve().parents[1] / "hardware/profiles"
DEFINITION_PATH = Path("usr/lib/tilo/hardware-profile-definition.json")
ARCH_CONFIG = {"x86_64": "CONFIG_X86_64=y", "aarch64": "CONFIG_ARM64=y"}
FIELDS = {"schema_version", "id", "architecture", "devicetree", "firmware"}
FIELDS_V2 = FIELDS | {"system_firmware"}
OPTIONAL_V2 = {"boot_modules", "always_on"}
PROFILE_PATH = Path("usr/lib/tilo/hardware-profile.json")
PROFILE_FIELDS = {"schema_version", "id", "architecture", "devicetree", "devicetree_sha256", "target_update_ref"}
DEFAULT_TARGET_IMGREF = "registry.tilo.so/tilo/tilo:latest"


def validate_target_imgref(value, *, root=None):
    """Production by default; development requires immutable image evidence."""
    if root is not None:
        bound = development_origin(root)
        if bound is not None:
            if value != bound:
                raise ValueError("update origin differs from immutable development origin")
            return value
        if isinstance(value, str) and re.fullmatch(
            r"registry\.tilo\.so/tilo/tilo-vm(?::[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}|@sha256:[0-9a-f]{64})", value
        ):
            # Current tilo-vm images intentionally admit unsigned development
            # updates. Both immutable flavour and the repository's actual
            # policy must agree; a live medium or environment variable alone
            # cannot enable this origin for a production target.
            flavour = trusted_path(root, Path("usr/lib/tilo/image-flavour"), regular=True)
            policy = trusted_path(root, Path("etc/containers/policy.json"), regular=True)
            if flavour.stat().st_size > 16 or flavour.read_bytes() != b"dev\n":
                raise ValueError("tilo-vm update origin requires the immutable dev flavour")
            if policy.stat().st_size > 65536:
                raise ValueError("development container policy is too large")
            admission = json.loads(policy.read_bytes()).get("transports", {}).get("docker", {}).get(
                "registry.tilo.so/tilo/tilo-vm")
            if admission != [{"type": "insecureAcceptAnything"}]:
                raise ValueError("tilo-vm update origin requires its development repository policy")
            return value
    # Board images carry private firmware and follow the forge's private
    # repository; every other image follows the public one. Both require
    # the release signature.
    if not isinstance(value, str) or not re.fullmatch(
        r"(?:registry\.tilo\.so|code\.tilo\.so)/tilo/tilo(?::[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}|@sha256:[0-9a-f]{64})", value
    ):
        raise ValueError("TILO_TARGET_IMGREF must explicitly name a valid tag or sha256 digest in a signature-required Tilo repository (registry.tilo.so/tilo/tilo, or code.tilo.so/tilo/tilo for board images)")
    return value


DEVELOPMENT_PATH = Path("usr/share/tilo/development-trust")
DEVELOPMENT_ORIGIN = DEVELOPMENT_PATH / "origin.json"


def development_target(value, architecture=None):
    """One explicitly selected development repository, never production."""
    match = re.fullmatch(r"([a-z0-9](?:[a-z0-9.-]*[a-z0-9])?)(?::([0-9]{1,5}))?/(tilo-development/os|tilo/tilo-development)(:[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}|@sha256:[0-9a-f]{64})", value or "")
    if not match:
        raise ValueError("development origin must name an approved development repository with an explicit tag or digest")
    host, port, path, selector = match.groups()
    if architecture is not None and architecture not in ARCH_CONFIG:
        raise ValueError("unsupported development image architecture")
    retained = host == "registry.tilo.so" and port is None and path == "tilo/tilo-development"
    if (host in ("registry.tilo.so", "code.tilo.so") or path == "tilo/tilo-development") and not retained:
        raise ValueError("retained development repository must use its exact HTTPS registry identity")
    if host == "localhost" or ".." in host or any(not part or part.startswith('-') or part.endswith('-') for part in host.split('.')):
        raise ValueError("development registry host is invalid or a production alias")
    if retained and selector.startswith(':'):
        tag_architecture = re.fullmatch(r"(?:[A-Za-z0-9_][A-Za-z0-9_.-]*-)?(x86_64|aarch64)", selector[1:])
        if tag_architecture is None or (architecture is not None and tag_architecture[1] != architecture):
            raise ValueError("retained development update tag must identify the matching native architecture")
    if port and (not 1 <= int(port) <= 65535 or str(int(port)) != port):
        raise ValueError("development registry port must be canonical and valid")
    registry = host + (":" + port if port else "")
    return {"target_update_ref": value, "repository": registry + "/" + path,
            "registry": registry, "tls_required": host != "127.0.0.1"}


def development_policy(repository):
    return {"default": [{"type": "reject"}], "transports": {"docker": {repository: [
        {"type": "sigstoreSigned", "keyPath": "/usr/share/tilo/cosign.pub",
         "signedIdentity": {"type": "matchRepository"}}]}}}


def development_origin(root):
    directory = root / DEVELOPMENT_PATH
    if not directory.exists() and not directory.is_symlink():
        return None
    def read(relative):
        path = trusted_path(root, relative, regular=True)
        if path.stat().st_size > 65536:
            raise ValueError("development origin input is too large")
        return path.read_bytes()
    document = json.loads(read(DEVELOPMENT_ORIGIN))
    if set(document) != {"schema", "purpose", "target_update_ref", "manifest_sha256"} or type(document['schema']) is not int or document['schema'] != 1 or document['purpose'] != 'tilo-development':
        raise ValueError("invalid immutable development origin metadata")
    selected = development_target(document['target_update_ref'])
    manifest_bytes = read(DEVELOPMENT_PATH / 'manifest.json')
    if hashlib.sha256(manifest_bytes).hexdigest() != document['manifest_sha256']:
        raise ValueError("development origin public manifest binding differs")
    provenance = json.loads(read(DEVELOPMENT_PATH / 'provenance.json'))
    if provenance.get('purpose') != 'tilo-development' or provenance.get('manifest_sha256') != document['manifest_sha256']:
        raise ValueError('development origin provenance differs')
    manifest = json.loads(manifest_bytes)
    if set(manifest) != {'schema', 'purpose', 'sha256'} or manifest['schema'] != 1 or manifest['purpose'] != 'tilo-development':
        raise ValueError("invalid development public manifest")
    names = {'cosign.pub', 'tpm2-pcr-signing.public.pem', 'efi-signing.crt', 'registry-owner.pub'}
    if set(manifest['sha256']) != names:
        raise ValueError("development public identities are incomplete")
    for name in names:
        if hashlib.sha256(read(DEVELOPMENT_PATH / name)).hexdigest() != manifest['sha256'][name]:
            raise ValueError("development public identity differs from manifest")
    for name in ('cosign.pub', 'tpm2-pcr-signing.public.pem'):
        if read(Path('usr/share/tilo') / name) != read(DEVELOPMENT_PATH / name):
            raise ValueError("installed development key differs from bound public identity")
    owner = read(DEVELOPMENT_PATH / 'registry-owner.pub').strip()
    installed_owner = read(Path('usr/share/tilo/trust/tilo-main-registry-owner.pub')).strip()
    if (not re.fullmatch(rb'[0-9a-f]{64}', owner) or owner == b'0' * 64
            or installed_owner != owner):
        raise ValueError("installed development registry owner differs from bound public identity")
    if json.loads(read(Path('etc/containers/policy.json'))) != development_policy(selected['repository']):
        raise ValueError("development update policy must require the bound signature identity and reject other origins")
    attachments = ('docker:\n  "' + selected['repository'] + '":\n    use-sigstore-attachments: true\n').encode()
    transport = ('[[registry]]\nlocation = "' + selected['registry'] + '"\ninsecure = ' + str(not selected['tls_required']).lower() + '\n').encode()
    if read(Path('etc/containers/registries.d/tilo-development.yaml')) != attachments or read(Path('etc/containers/registries.conf.d/99-tilo-development.conf')) != transport:
        raise ValueError("development registry transport or signature attachment configuration differs")
    return document['target_update_ref']


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
    validate_target_imgref(data["target_update_ref"], root=root)
    if development_origin(root) is not None:
        development_target(data["target_update_ref"], data["architecture"])
    return data


def install_update_origin(root, override=""):
    bound_development = development_origin(root)
    data = read_profile(root)
    if data is None:
        return validate_target_imgref(override or bound_development or DEFAULT_TARGET_IMGREF, root=root)
    bound = data["target_update_ref"]
    if override and override != bound:
        raise ValueError("TILO_TARGET_IMGREF conflicts with the image-owned hardware profile update reference")
    return bound


def profile_id(value):
    if not isinstance(value, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", value):
        raise ValueError("invalid hardware profile id")
    return value


def relative_path(value):
    if not isinstance(value, str) or not value or any(
        part in ("", ".", "..") or not re.fullmatch(r"[A-Za-z0-9_.+,-]+", part)
        for part in value.split("/")
    ):
        raise ValueError("hardware definition path must be a safe relative path")
    return Path(value)


def validate_definition(data):
    if not isinstance(data, dict):
        raise ValueError("hardware definition has missing or unknown fields")
    schema = data.get("schema_version")
    if schema == 1:
        expected_fields, optional_fields = FIELDS, set()
    elif schema == 2:
        expected_fields, optional_fields = FIELDS_V2, OPTIONAL_V2
    else:
        expected_fields, optional_fields = None, set()
    if expected_fields is None or set(data) - (expected_fields | optional_fields) or expected_fields - set(data):
        if type(schema) is not int or schema not in (1, 2):
            raise ValueError("unsupported hardware definition schema")
        raise ValueError("hardware definition has missing or unknown fields")
    if type(schema) is not int:
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
    system_firmware = data.get("system_firmware", [])
    if not isinstance(system_firmware, list) or len(system_firmware) > 256:
        raise ValueError("hardware definition system firmware must be a bounded path list")
    system_paths = set()
    for path in system_firmware:
        relative_path(path)
        if any(path == other or path.startswith(other + "/") or other.startswith(path + "/")
               for other in system_paths):
            raise ValueError("hardware definition system firmware paths must be unique and must not overlap")
        if any(path == destination or path.startswith(destination + "/") or destination.startswith(path + "/")
               for destination in destinations):
            raise ValueError("system firmware must not overlap local firmware destinations")
        system_paths.add(path)
    boot_modules = data.get("boot_modules", [])
    if not isinstance(boot_modules, list) or len(boot_modules) > 64:
        raise ValueError("hardware definition boot modules must be a bounded name list")
    seen_modules = set()
    for name in boot_modules:
        if not isinstance(name, str) or not re.fullmatch(r"[a-z0-9][a-z0-9_-]{0,63}", name):
            raise ValueError("hardware definition boot module name is invalid")
        if name in seen_modules:
            raise ValueError("hardware definition boot module names must be unique")
        seen_modules.add(name)
    if type(data.get("always_on", False)) is not bool:
        raise ValueError("hardware definition always_on must be a boolean")
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
        if len(sys.argv) == 3 and sys.argv[1] == "--development-target":
            print(development_target(sys.argv[2])["target_update_ref"])
            return
        development_build = len(sys.argv) == 7 and sys.argv[-1] == "--development-trust"
        if development_build:
            sys.argv.pop()
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
        if development_build:
            development_target(target_update_ref, architecture)
        else:
            validate_target_imgref(target_update_ref)
    except (OSError, ValueError, TypeError) as error:
        raise SystemExit(f"hardware profile rejected: {error}") from error


if __name__ == "__main__":
    main()
