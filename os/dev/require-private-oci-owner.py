#!/usr/bin/env python3
"""Check the board OCI owner, with an explicit public-release override."""

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


def owner_is_approved(payload: object, allow_public: bool) -> bool:
    return owner_is_private(payload) or (
        allow_public
        and isinstance(payload, dict)
        and payload.get("username") == "tilo"
        and payload.get("visibility") == "public"
    )


def fetch_owner(headers: dict[str, str]) -> object | None:
    connection = HTTPSConnection(OWNER_HOST, timeout=15)
    try:
        connection.request("GET", OWNER_PATH, headers=headers)
        response = connection.getresponse()
        if response.status != 200:
            return None
        return json.loads(response.read(65537))
    except Exception:
        return None
    finally:
        connection.close()


def main() -> int:
    token = os.environ.get("REGISTRY_TOKEN", "")
    if not token:
        print("Lenovo publication requires an authenticated owner-visibility check", file=sys.stderr)
        return 1

    allow_public = os.environ.get("TILO_ALLOW_PUBLIC_LENOVO_FIRMWARE") == "true"
    owner = fetch_owner({"Authorization": f"token {token}"})
    if owner is None and allow_public:
        # Some write-scoped registry tokens cannot read the public org API.
        owner = fetch_owner({})
    if owner is None:
        print("Could not verify the OCI owner's private visibility", file=sys.stderr)
        return 1

    if not owner_is_approved(owner, allow_public):
        print("Refusing to publish Lenovo firmware to an unapproved OCI owner", file=sys.stderr)
        return 1
    print(f"Verified {owner['visibility']} OCI owner tilo for Lenovo firmware publication")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
