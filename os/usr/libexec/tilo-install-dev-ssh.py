#!/usr/bin/env python3
"""Apply explicit owner SSH keys to a self-installed development image.

The installer calls this only for a dev self-install with a setup handoff.
It does not create a personal identity or change onboarding state.
"""

import argparse
import base64
import os
from pathlib import Path
import re
import subprocess


MAX_KEYS = 16


def public_keys(path):
    """The request's keys, one OpenSSH public key per line, normalized."""
    info = path.lstat()
    if not path.is_file() or path.is_symlink() or info.st_nlink != 1 or info.st_size > 32768:
        raise ValueError("SSH key must be a bounded, non-linked regular file")
    text = path.read_text(encoding="ascii")
    if "\r" in text:
        raise ValueError("expected SSH public keys without authorized_keys options")
    lines = [line.strip() for line in text.split("\n") if line.strip()]
    if not lines or len(lines) > MAX_KEYS:
        raise ValueError(f"expected 1 to {MAX_KEYS} SSH public keys")
    keys = []
    for line in lines:
        parts = line.split()
        if len(parts) < 2:
            raise ValueError("expected SSH public keys without authorized_keys options")
        if parts[0] not in ("ssh-ed25519", "ssh-rsa", "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp384", "ecdsa-sha2-nistp521"):
            raise ValueError("unsupported SSH public key type")
        blob = base64.b64decode(parts[1], validate=True)
        length = int.from_bytes(blob[:4], "big")
        if blob[4:4 + length] != parts[0].encode():
            raise ValueError("SSH public key type differs from its wire encoding")
        keys.append(" ".join(parts[:2]) + "\n")
    subprocess.run(["ssh-keygen", "-l", "-f", str(path)], check=True, capture_output=True)
    return "".join(keys)


def directory(root, relative):
    current = root
    for part in Path(relative).parts:
        current /= part
        if current.is_symlink():
            raise ValueError("installed SSH path contains a symlink")
        current.mkdir(mode=0o755, exist_ok=True)
        if not current.is_dir():
            raise ValueError("installed SSH path is not a directory")
    return current


def label(path, installed):
    context = subprocess.run(["/usr/sbin/matchpathcon", "-n", installed],
                             check=True, capture_output=True, text=True).stdout.strip()
    subprocess.run(["chcon", "--no-dereference", context, str(path)], check=True)


def apply(etc, username, key_path, labeler=label):
    if etc.is_symlink() or not etc.is_dir() or not re.fullmatch(r"[a-z_][a-z0-9_-]{0,31}", username):
        raise ValueError("invalid installed /etc or account")
    key = public_keys(key_path)
    keys = directory(etc, "ssh/authorized_keys")
    config = directory(etc, "ssh/sshd_config.d")
    wants = directory(etc, "systemd/system/multi-user.target.wants")
    for path in (keys, config, wants):
        labeler(path, "/etc/" + path.relative_to(etc).as_posix())
    files = {
        keys / username: key,
        config / "00-tilo-bringup.conf": (
            f"AllowUsers {username}\nPermitRootLogin no\nPasswordAuthentication no\n"
            "KbdInteractiveAuthentication no\nPubkeyAuthentication yes\n"
            "AuthorizedKeysFile /etc/ssh/authorized_keys/%u\n"
        ),
    }
    # These are request-specific outputs. Refuse unrelated or linked state.
    for path in files:
        if path.exists() or path.is_symlink():
            raise ValueError("SSH provisioning output already exists")
    enable = wants / "sshd.service"
    if enable.exists() or enable.is_symlink():
        raise ValueError("SSH enablement must first be cleared by the installer")
    for path, content in files.items():
        with path.open("x", encoding="ascii") as stream:
            os.chmod(path, 0o644)
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        labeler(path, "/etc/" + path.relative_to(etc).as_posix())
    enable.symlink_to("/usr/lib/systemd/system/sshd.service")
    labeler(enable, "/etc/systemd/system/multi-user.target.wants/sshd.service")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--etc", type=Path, required=True)
    parser.add_argument("--username", required=True)
    parser.add_argument("--key", type=Path, required=True)
    args = parser.parse_args()
    apply(args.etc, args.username, args.key)
