#!/usr/bin/env python3
"""Prompt for distinct retained VM secrets and send only JSON on stdout."""

import getpass
import json
import sys


def main():
    account = getpass.getpass("Retained account password: ", stream=sys.stderr)
    disk = getpass.getpass("Retained disk passphrase: ", stream=sys.stderr)
    if account == disk or not account or not disk:
        print("account password and disk passphrase must be distinct and nonempty", file=sys.stderr)
        return 1
    print(json.dumps({"account_password": account, "disk_passphrase": disk},
                     separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
