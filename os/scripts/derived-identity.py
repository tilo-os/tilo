#!/usr/bin/env python3
"""A signing identity derived from what it signs, for development images.

    derived-identity.py PURPOSE PATH... > identity.json

Prints `{"secret_key_hex": "<64 hex>"}`, the identity shape tilo-runtime-bundle and
tilo-app-package take, whose key is a hash of PURPOSE and the contents of every
PATH (files, and every file below directories, by path and bytes; missing paths
count as absent). Ed25519 signs deterministically, so the same payload signed
with the same identity gives the same bytes: a development image's runtime bundle
and app store are then the same layers in every build of the same binaries,
where a key minted per build made them new each time.

The key is no secret: anyone holding the payload can derive it. That is what a
development image needs, a signature that says which build of a payload this is,
and no more: only that image's own trust list names it, and releases sign with
their own identities.
"""

import hashlib
import json
import os
import stat
import sys


def feed(h, path, label):
    try:
        st = os.lstat(path)
    except FileNotFoundError:
        h.update(b"absent\0" + label.encode() + b"\0")
        return
    if stat.S_ISDIR(st.st_mode):
        h.update(b"dir\0" + label.encode() + b"\0")
        for name in sorted(os.listdir(path)):
            feed(h, os.path.join(path, name), label + "/" + name)
    elif stat.S_ISLNK(st.st_mode):
        h.update(b"link\0" + label.encode() + b"\0" + os.readlink(path).encode() + b"\0")
    else:
        h.update(b"file\0" + label.encode() + b"\0" + (b"x" if st.st_mode & 0o111 else b"-"))
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        h.update(b"\0")


def main(argv):
    if len(argv) < 2:
        sys.exit("usage: derived-identity.py PURPOSE PATH...")
    h = hashlib.sha256(b"tilo-derived-identity/v1\0" + argv[0].encode() + b"\0")
    for p in argv[1:]:
        feed(h, p, os.path.basename(p.rstrip("/")))
    print(json.dumps({"secret_key_hex": h.hexdigest()}))


if __name__ == "__main__":
    main(sys.argv[1:])
