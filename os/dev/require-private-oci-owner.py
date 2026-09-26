#!/usr/bin/env python3
"""Refuse publication of bundled board firmware to a public OCI owner."""

import json
import os
import sys
from http.client import HTTPSConnection


OWNER_HOST = "code.tilo.so"
OWNER_PATH = "/api/v1/orgs/tilo"


def owner_is_private(payload: object) -> bool:
    return (
        isinstance(payload, dict)
        and payload.get("username") == "tilo"
        and payload.get("visibility") == "private"
    )


def main() -> int:
    token = os.environ.get("REGISTRY_TOKEN", "")
    if not token:
        print("Lenovo publication requires an authenticated owner-visibility check", file=sys.stderr)
        return 1

    connection = HTTPSConnection(OWNER_HOST, timeout=15)
    try:
        connection.request("GET", OWNER_PATH, headers={"Authorization": f"token {token}"})
        response = connection.getresponse()
        if response.status != 200:
            raise ValueError("owner endpoint did not return 200")
        owner = json.loads(response.read(65537))
    except Exception:
        print("Could not verify the OCI owner's private visibility", file=sys.stderr)
        return 1
    finally:
        connection.close()

    if not owner_is_private(owner):
        print("Refusing to publish Lenovo firmware to a non-private OCI owner", file=sys.stderr)
        return 1
    print("Verified private OCI owner tilo for Lenovo firmware publication")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
