#!/usr/bin/env python3
"""Select a versioned installer catalog and bind its entries to release files."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys
from urllib.parse import urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "os/dev"))
from hardware_profile import validate_target_imgref


def select(root):
    matches = sorted(Path(root).rglob("tilo-installer-catalog-v*.json"))
    if len(matches) != 1:
        raise ValueError(f"expected exactly one installer catalog, found {len(matches)}")
    path = matches[0]
    if path.is_symlink() or not path.is_file():
        raise ValueError("installer catalog must be a regular file")
    catalog = json.loads(path.read_text(encoding="utf-8"))
    schema = catalog.get("schema")
    if type(schema) is not int or schema not in (1, 2):
        raise ValueError("unsupported installer catalog schema")
    if path.name != f"tilo-installer-catalog-v{schema}.json":
        raise ValueError("installer catalog filename does not match its schema")
    return path, catalog


INTERNAL_PURPOSE = "tilo-internal-installer"
INTERNAL_KEYS = {"purpose", "layout", "release_id", "arch", "manifest_url", "manifest_sha256", "manifest_size"}
TARGET_POLICY_PURPOSE = "tilo-target-policy"
TARGET_POLICY_KEYS = {"purpose", "layout", "release_id", "arch", "generation",
                      "target_uki_sha256", "url", "archive_sha256", "archive_size"}


def target_policy(entry, tag):
    """Shape of a CatalogRelease::TargetPolicy entry; it travels beside media."""
    if (set(entry) != TARGET_POLICY_KEYS or entry["layout"] != "target-policy-bundle-v1"
            or entry["release_id"] != tag):
        raise ValueError("invalid target policy catalog entry")
    if entry["arch"] not in ("x86_64", "aarch64") or type(entry["generation"]) is not int or entry["generation"] <= 0:
        raise ValueError("invalid target policy architecture or generation")
    if not isinstance(entry["target_uki_sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", entry["target_uki_sha256"]):
        raise ValueError("invalid target policy UKI digest")
    if not isinstance(entry["archive_sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", entry["archive_sha256"]):
        raise ValueError("invalid target policy archive digest")
    if type(entry["archive_size"]) is not int or entry["archive_size"] <= 0:
        raise ValueError("invalid target policy archive size")
    url = urlsplit(entry["url"])
    if (url.scheme != "https" or url.hostname != "code.tilo.so" or url.port not in (None, 443)
            or url.username or url.password or url.query or url.fragment):
        raise ValueError("target policy URL is not the trusted package origin")


def internal(entry, tag):
    """Shape of a CatalogRelease::Internal entry (build-release-catalog.sh binds its bytes)."""
    if set(entry) != INTERNAL_KEYS or entry["layout"] != "internal-bundle-v2" or entry["release_id"] != tag:
        raise ValueError("invalid internal installer catalog entry")
    arch = entry["arch"]
    owner = os.environ.get("TILO_INTERNAL_BUNDLE_OWNER", "tilo")
    expected = (f"https://code.tilo.so/api/packages/{owner}/generic/tilo-internal-installer/"
                f"{tag}/tilo-internal-installer-{arch}.json")
    if arch not in ("x86_64", "aarch64") or entry["manifest_url"] != expected:
        raise ValueError("internal installer manifest URL does not match its package origin")
    if (not isinstance(entry["manifest_sha256"], str)
            or not re.fullmatch(r"[0-9a-f]{64}", entry["manifest_sha256"])
            or type(entry["manifest_size"]) is not int or not 0 < entry["manifest_size"] <= 65536):
        raise ValueError("invalid internal installer manifest digest or size")


def media(catalog, root, arch=None):
    if not re.fullmatch(r"imager-v[0-9]+\.[0-9]+\.[0-9]+", catalog.get("tag", "")):
        raise ValueError("installer catalog requires a release tag")
    entries = catalog.get("releases")
    if not isinstance(entries, list) or not entries:
        raise ValueError("installer catalog has no media")
    repository = os.environ.get("TILO_RELEASE_REPOSITORY", "tilo-os/releases")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("release repository must be OWNER/REPOSITORY")
    server = urlsplit(os.environ.get("TILO_RELEASE_SERVER", "https://github.com"))
    if (server.scheme != "https" or not server.hostname or server.username or server.password
            or server.path not in ("", "/") or server.query or server.fragment):
        raise ValueError("release server must be an HTTPS origin without credentials")
    # Accessing port also rejects malformed/out-of-range configured ports.
    _ = server.port
    identities = set()
    result = []
    for entry in entries:
        if isinstance(entry, dict) and entry.get("purpose") == INTERNAL_PURPOSE:
            # Internal installer bundles are not media: they are published as
            # Forgejo packages, never signed or shipped as release downloads.
            internal(entry, catalog["tag"])
            continue
        if isinstance(entry, dict) and entry.get("purpose") == TARGET_POLICY_PURPOSE:
            # The target policy is a small signed boot bundle, not an OS medium.
            target_policy(entry, catalog["tag"])
            continue
        target = entry.get("arch")
        profile = entry.get("hardware_profile", "generic")
        if target not in ("x86_64", "aarch64") or not isinstance(profile, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", profile):
            raise ValueError("invalid installer architecture or hardware profile")
        if catalog["schema"] == 1 and profile != "generic":
            raise ValueError("board media requires catalog schema 2")
        if catalog["schema"] == 2 and "hardware_profile" not in entry:
            raise ValueError("schema 2 requires an explicit hardware profile")
        if profile != "generic":
            provenance = entry.get("provenance")
            if not isinstance(provenance, dict):
                raise ValueError("board medium requires provenance")
            if not re.fullmatch(r"sha256:[0-9a-f]{64}", provenance.get("source_image_id", "")):
                raise ValueError("invalid board source image identity")
            for field in ("input_qcow2_sha256", "hardware_profile_sha256"):
                if not re.fullmatch(r"[0-9a-f]{64}", provenance.get(field, "")):
                    raise ValueError(f"invalid board provenance {field}")
            validate_target_imgref(provenance.get("target_update_ref"))
        identity = (target, profile)
        if identity in identities:
            raise ValueError("duplicate installer architecture and hardware profile")
        identities.add(identity)
        suffix = "" if profile == "generic" else f"-{profile}"
        name = f"tilo-installer-linux-{target}{suffix}.img.zst"
        url = urlsplit(entry.get("url", ""))
        expected_path = f"/{repository}/releases/download/{catalog['tag']}/{name}"
        if (url.scheme != server.scheme or url.netloc != server.netloc or url.username
                or url.password or url.query or url.fragment or url.path != expected_path):
            raise ValueError("catalog medium URL does not match the configured release origin, tag and hardware profile")
        for field in ("archive_sha256", "image_sha256"):
            if not isinstance(entry.get(field), str) or not re.fullmatch(r"[0-9a-f]{64}", entry[field]):
                raise ValueError(f"invalid catalog {field}")
        for field in ("archive_size", "image_size"):
            if type(entry.get(field)) is not int or entry[field] <= 0:
                raise ValueError(f"invalid catalog {field}")
        if arch is not None and target != arch:
            continue
        matches = sorted(Path(root).rglob(name))
        if len(matches) != 1 or matches[0].is_symlink() or not matches[0].is_file():
            raise ValueError(f"expected exactly one regular catalog medium {name}")
        path = matches[0]
        if path.stat().st_size != entry["archive_size"]:
            raise ValueError(f"catalog archive size mismatch: {name}")
        with path.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        if digest != entry["archive_sha256"]:
            raise ValueError(f"catalog archive digest mismatch: {name}")
        result.append(path)
    if not {("x86_64", "generic"), ("aarch64", "generic")} <= identities:
        raise ValueError("catalog lacks the required generic installer matrix")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("catalog_root", type=Path)
    parser.add_argument("--copy-to", type=Path)
    parser.add_argument("--media-root", type=Path)
    parser.add_argument("--arch", choices=("x86_64", "aarch64"))
    args = parser.parse_args()
    if args.copy_to and args.media_root:
        parser.error("select either embedding or media verification")
    if args.arch and not args.media_root:
        parser.error("--arch requires --media-root")
    try:
        path, catalog = select(args.catalog_root)
        if args.copy_to:
            shutil.copyfile(path, args.copy_to)
        elif args.media_root:
            for medium in media(catalog, args.media_root, args.arch):
                print(medium)
        else:
            print(path)
    except (OSError, ValueError, TypeError, AttributeError) as error:
        parser.exit(1, f"installer catalog: {error}\n")


if __name__ == "__main__":
    main()
