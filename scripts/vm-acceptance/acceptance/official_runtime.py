"""Read-only installed identity using canonical signature verifiers and OS trust.

Executed as the owning slot user; never installs, grants trust, or launches.
The SHA inventory bridges verified signed bundle bytes to kernel map_files bytes.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tomllib


def command(*args):
    result = subprocess.run(args, check=True, capture_output=True, text=True, timeout=120)
    return result.stdout


def fields(text):
    result = {}
    for line in text.splitlines():
        key, separator, value = line.partition(":")
        if separator:
            key = key.strip()
            if key in result:
                raise RuntimeError("canonical inspector returned duplicate fields")
            result[key] = value.strip()
    return result


def stamp(status):
    return (status.st_dev, status.st_ino, status.st_size, status.st_mtime_ns, status.st_ctime_ns)


def hash_file(file):
    with file.open("rb") as source:
        before = os.fstat(source.fileno())
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= 512 * 1024**2:
            raise RuntimeError("verified public file exceeds bound")
        digest, count = hashlib.sha256(), 0
        while chunk := source.read(1024 * 1024):
            count += len(chunk)
            if count > before.st_size:
                raise RuntimeError("verified public file grew while hashing")
            digest.update(chunk)
        if count != before.st_size or stamp(before) != stamp(os.fstat(source.fileno())):
            raise RuntimeError("verified public file changed while hashing")
    return digest.hexdigest(), before


def inspect(package, root, version, runtime, publisher):
    if (package != "so.tilo.calculator" or not re.fullmatch(r"[0-9a-f]{64}", root)
            or not re.fullmatch(r"[0-9a-f]{64}", publisher)
            or not re.fullmatch(r"tilo-runtime/[A-Za-z0-9.+_-]+", runtime)):
        raise RuntimeError("unsupported official package identity")
    command("/usr/bin/tilo-install", "verify", package)
    identity = fields(command("/usr/bin/tilo-install", "inspect", package))
    expected = {"id": package, "version": version, "package-id": "tapp:" + root,
                "publisher": "ed25519:" + publisher, "runtime": runtime + " (installed)"}
    if any(identity.get(key) != value for key, value in expected.items()):
        raise RuntimeError("installed package differs from reviewed official identity")
    tool = "/usr/bin/tilo-runtime-bundle"
    tag = runtime.split("/", 1)[1]
    bundle = Path(command(tool, "path", "--version", tag).strip())
    manifest_path = bundle / "bundle.toml"
    before_manifest = manifest_path.stat()
    if before_manifest.st_size > 1024 * 1024:
        raise RuntimeError("signed runtime manifest exceeds bound")
    manifest_bytes = manifest_path.read_bytes()
    manifest = tomllib.loads(manifest_bytes.decode())
    trust = Path("/etc/tilo/runtime-bundle-trusted-publishers.txt")
    status = trust.lstat()
    if not stat.S_ISREG(status.st_mode) or status.st_uid != 0 or status.st_mode & 0o022:
        raise RuntimeError("independent OS runtime trust file unavailable")
    trusted = {line.strip() for line in trust.read_text().splitlines()
               if line.strip() and not line.strip().startswith("#")}
    key = manifest.get("publisher_hex")
    if key not in trusted or manifest.get("version") != tag or manifest.get("arch") != "aarch64":
        raise RuntimeError("official runtime does not match independent OS trust or target")
    verify = (tool, "verify", str(bundle), "--require-signature", "--require-publisher", key,
              "--require-arch", "aarch64")
    command(*verify)
    files, stamps, total_bytes = {}, {}, 0
    entries = manifest.get("files", [])
    if not 0 < len(entries) <= 256:
        raise RuntimeError("bounded official runtime inventory unavailable")
    for entry in entries:
        relative = Path(entry["path"])
        if relative.is_absolute() or ".." in relative.parts:
            raise RuntimeError("signed runtime path escapes bundle")
        file = bundle / relative
        digest, before = hash_file(file)
        total_bytes += before.st_size
        if before.st_size != entry["size"] or total_bytes > 2 * 1024**3:
            raise RuntimeError("official runtime inventory differs or exceeds total byte bound")
        path = f"/run/tilo-runtime/{tag}/{relative.as_posix()}"
        if path in files:
            raise RuntimeError("duplicate signed runtime file")
        files[path], stamps[file] = digest, before
    app_manifest_path = Path(identity["manifest"])
    app_manifest_stamp = app_manifest_path.stat()
    if app_manifest_stamp.st_size > 1024 * 1024:
        raise RuntimeError("installed app manifest exceeds bound")
    app_manifest_bytes = app_manifest_path.read_bytes()
    app_manifest = tomllib.loads(app_manifest_bytes.decode())
    executable = Path(app_manifest["entrypoint"]["bin"])
    if executable.is_absolute() or ".." in executable.parts:
        raise RuntimeError("installed app executable escapes payload")
    executable_path = Path(identity["payload-root"]) / executable
    executable_sha, executable_stamp = hash_file(executable_path)
    command("/usr/bin/tilo-install", "verify", package)
    after_identity = fields(command("/usr/bin/tilo-install", "inspect", package))
    if (identity != after_identity and any(identity.get(key) != after_identity.get(key)
            for key in (*expected, "payload-root", "manifest"))):
        raise RuntimeError("installed package selection changed across verification")
    if (stamp(app_manifest_stamp) != stamp(app_manifest_path.stat())
            or app_manifest_bytes != app_manifest_path.read_bytes()
            or stamp(executable_stamp) != stamp(executable_path.stat())):
        raise RuntimeError("installed executable changed across signature verification")
    # Re-run the canonical verifier over the same inventory, then require every
    # inspected object and manifest to remain stable across both verifications.
    command(*verify)
    if (stamp(before_manifest) != stamp(manifest_path.stat()) or manifest_bytes != manifest_path.read_bytes()
            or any(stamp(file.stat()) != stamp(status) for file, status in stamps.items())):
        raise RuntimeError("official runtime changed across canonical verification")
    return {"package": expected, "runtime": runtime, "publisher": key,
            "bundle_root": manifest["root_hash"], "files": files, "executable_sha256": executable_sha,
            "verification": "tilo-install-verify-and-os-pinned-runtime-bundle", "uid": os.getuid()}


if __name__ == "__main__":
    print(json.dumps(inspect(*sys.argv[1:])))
