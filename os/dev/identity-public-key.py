#!/usr/bin/env python3
"""Read a runtime/app identity JSON on stdin and print only its Ed25519 public key."""

import json
import re
import subprocess
import sys


PKCS8_PREFIX = bytes.fromhex("302e020100300506032b657004220420")
SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")


def public_key(identity: bytes) -> str:
    data = json.loads(identity)
    secret = data["secret_key_hex"]
    if not isinstance(secret, str) or re.fullmatch(r"[0-9a-fA-F]{64}", secret) is None:
        raise ValueError("identity must contain a 32-byte secret_key_hex")
    result = subprocess.run(
        ["openssl", "pkey", "-inform", "DER", "-pubout", "-outform", "DER"],
        input=PKCS8_PREFIX + bytes.fromhex(secret),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    if not result.stdout.startswith(SPKI_PREFIX) or len(result.stdout) != len(SPKI_PREFIX) + 32:
        raise ValueError("OpenSSL did not return an Ed25519 public key")
    return result.stdout[len(SPKI_PREFIX):].hex()


if __name__ == "__main__":
    try:
        print(public_key(sys.stdin.buffer.read()))
    except FileNotFoundError:
        print("OpenSSL is required on the CI build host to check signing identities", file=sys.stderr)
        raise SystemExit(1) from None
    except (ValueError, KeyError, subprocess.CalledProcessError):
        print("invalid signing identity", file=sys.stderr)
        raise SystemExit(1) from None
