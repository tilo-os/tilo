#!/usr/bin/env python3
"""Launch the existing Files daemon only for saved, authorized primary state."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


def paths(environment):
    home = Path(environment["HOME"])
    state_home = environment.get("XDG_STATE_HOME") or home / ".local/state"
    data_home = environment.get("XDG_DATA_HOME") or home / ".local/share"
    config_home = environment.get("XDG_CONFIG_HOME") or home / ".config"
    state = Path(state_home) / "tilo/files"
    identity = Path(environment.get("TILO_IDENTITY_ROOT") or Path(data_home) / "tilo")
    config = Path(config_home) / "tilo"
    runtime = Path(environment["XDG_RUNTIME_DIR"]) / "tilo/files-workspace"
    for path in (state, identity, config, runtime):
        if not path.is_absolute():
            raise ValueError("Files paths must be absolute")
    return state, identity, config, runtime


def command(environment, fs_binary, admin_binary):
    state, identity, config, runtime = paths(environment)
    if not (state / "namespace.bin").is_file():
        raise ValueError("no saved Files namespace; recover or explicitly initialize first")
    # This production check validates the existing certificate/key, signed
    # authority and exact Files capability. It never creates credentials.
    subprocess.run([admin_binary, "files-workspace-check", "--identity-root", str(identity), "--cache-dir", str(state)], check=True, stdout=subprocess.DEVNULL)
    status = subprocess.run([fs_binary, "workspace-status", "--cache-dir", str(state)], check=True, capture_output=True)
    if not json.loads(status.stdout)["can_resume_existing"]:
        raise ValueError("saved workspace cannot be resumed")
    mount = state / "mount"
    mount.mkdir(mode=0o700, exist_ok=True)
    runtime.mkdir(mode=0o700, parents=True, exist_ok=True)
    child_environment = dict(environment)
    child_environment["TILO_IDENTITY_ROOT"] = str(identity)
    child_environment["TILO_AUTHORITY_SOCKET_DIR"] = str(runtime)
    argv = [fs_binary, "serve", "--require-existing-workspace", "--cache-dir", str(state), "--mount-point", str(mount), "--acl", str(config / "fs-capabilities.json"), "--data-plane", "fuse", "--storage-sock", str(runtime / "fs-storage.sock"), "--blob-sock", str(runtime / "fs-blobs.sock")]
    return argv, child_environment


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--fs-binary", default="/usr/bin/fs")
    parser.add_argument("--admin-binary", default="/usr/bin/tilo-relay-admin")
    args = parser.parse_args()
    try:
        argv, environment = command(os.environ, args.fs_binary, args.admin_binary)
    except (KeyError, ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"Files service is not ready: {error}", file=sys.stderr)
        return 78
    if args.dry_run:
        print(json.dumps(argv))
        return 0
    os.execve(argv[0], argv, environment)


if __name__ == "__main__":
    sys.exit(main())
