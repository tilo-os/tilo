#!/usr/bin/env python3
"""Build the embedded Desktop catalog from local artifacts and reviewed evidence.

Usage: python3 build-desktop-catalog.py MANIFEST --output CATALOG

The manifest has schema=1, an explicit tag, and releases. Each release supplies
id, version (EXACT package-manager version, including revision/epoch if applicable),
distro_id, distro_version, architecture, package_format, url, artifact, and
verified_runtime. Relative artifact and evidence reference paths resolve against
the manifest directory. byte_size/sha256 are forbidden inputs: they are measured.

verified_runtime has schema=1, reference (an existing nonempty native report),
package_sha256, and true graphical_login, return_to_prior_session, identity_setup,
selected_folder_import, source_intact fields. This is a review input, NOT an
automatic proof of those claims. Reviewers must inspect actual native evidence
before including this catalog in a signed application. Building an artifact or
running unit tests cannot supply these attestations. All generated nonempty
releases require this evidence; package availability never implies runtime proof.

An empty catalog must use tag=unreleased. This tool neither publishes artifacts
nor contacts release servers. It emits only explicitly supplied release URLs.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tempfile
from urllib.parse import urlsplit


RELEASE_FIELDS = {"id", "version", "distro_id", "distro_version", "architecture", "package_format", "url", "artifact", "verified_runtime"}
EVIDENCE_CHECKS = {"graphical_login", "return_to_prior_session", "identity_setup", "selected_folder_import", "source_intact"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def fields(value, expected, label):
    require(isinstance(value, dict) and set(value) == expected, f"{label}: missing or unknown fields")


def text(value):
    return isinstance(value, str) and bool(value) and value == value.strip() and not any(ord(c) < 32 or ord(c) == 127 for c in value)


def local_path(base, value):
    require(text(value), "Invalid local artifact/evidence path")
    return base / value


def file_digest(path):
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    with os.fdopen(os.open(path, flags), "rb") as artifact:
        before = os.fstat(artifact.fileno())
        require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= 2**63 - 1, f"Not a nonempty regular file: {path}")
        digest = hashlib.sha256()
        count = 0
        while chunk := artifact.read(1024 * 1024):
            count += len(chunk)
            require(count <= before.st_size, f"Artifact changed while hashing: {path}")
            digest.update(chunk)
        after = os.fstat(artifact.fileno())
        require(count == before.st_size and (before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
                (after.st_size, after.st_mtime_ns, after.st_ctime_ns), f"Artifact changed while hashing: {path}")
        return count, digest.hexdigest()


def trusted_url(value):
    url = urlsplit(value)
    require(url.scheme == "https" and url.hostname == "code.tilo.so" and url.port in (None, 443)
            and url.username is None and url.password is None and not url.fragment, "Untrusted release URL")
    return value


def build(manifest, base):
    fields(manifest, {"schema", "tag", "releases"}, "Manifest")
    require(type(manifest["schema"]) is int and manifest["schema"] == 1, "Unsupported manifest schema")
    require(text(manifest["tag"]) and isinstance(manifest["releases"], list), "Invalid tag/releases")
    require((manifest["tag"] == "unreleased") == (not manifest["releases"]), "Empty catalog must be explicitly unreleased")
    output = []
    selectors, ids = set(), set()
    for release in manifest["releases"]:
        fields(release, RELEASE_FIELDS, "Release")
        require(all(text(release[key]) for key in ("id", "version", "distro_id", "distro_version", "url")), "Invalid release strings")
        require(re.fullmatch(r"[a-z0-9_-]+", release["distro_id"]) is not None, "Invalid distro_id")
        require(release["architecture"] in ("aarch64", "x86_64"), "Unsupported architecture")
        require(release["package_format"] in ("rpm", "deb", "arch", "nix_closure"), "Unsupported package format")
        trusted_url(release["url"])
        selector = (release["distro_id"], release["distro_version"], release["architecture"])
        require(selector not in selectors and release["id"] not in ids, "Duplicate release id/platform selector")
        selectors.add(selector)
        ids.add(release["id"])
        size, digest = file_digest(local_path(base, release["artifact"]))
        evidence = release["verified_runtime"]
        fields(evidence, {"schema", "reference", "package_sha256"} | EVIDENCE_CHECKS, "Runtime evidence")
        require(type(evidence["schema"]) is int and evidence["schema"] == 1, "Unsupported evidence schema")
        require(evidence["package_sha256"] == digest, "Runtime evidence does not match artifact digest")
        require(all(evidence[key] is True for key in EVIDENCE_CHECKS), "Runtime evidence must explicitly complete every native check")
        file_digest(local_path(base, evidence["reference"]))
        output.append({key: value for key, value in release.items() if key != "artifact"} | {"byte_size": size, "sha256": digest})
    return {"schema": 1, "tag": manifest["tag"], "releases": output}


def reject_duplicate_fields(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"Duplicate JSON field: {key}")
        result[key] = value
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    temporary = None
    try:
        manifest = json.loads(args.manifest.read_text(), object_pairs_hook=reject_duplicate_fields)
        result = build(manifest, args.manifest.parent)
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=args.output.parent, prefix=".desktop-catalog-", delete=False) as target:
            temporary = Path(target.name)
            json.dump(result, target, indent=2)
            target.write("\n")
            target.flush()
            os.fsync(target.fileno())
        os.replace(temporary, args.output)
    except (OSError, ValueError, TypeError) as error:
        parser.exit(1, f"Desktop catalog generation refused: {error}\n")
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
