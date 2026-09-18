#!/usr/bin/python3
"""External signer CLI adapter. Install with signing_spool.py outside checkout.

Adjacent bridge.json is root-owned; it contains only public configuration.
Operator transports fixed request/response files over an authenticated channel.
"""
from pathlib import Path
import sys
from signing_spool import remote_sign, signer


def main():
    args = sys.argv[1:]
    if not args or (args[0] == 'export-efi-cert' and len(args) != 2) or (args[0] != 'export-efi-cert' and len(args) != 3):
        raise ValueError('expected signer operation and file arguments')
    executable = signer.trusted(Path(__file__).absolute())
    remote_sign(executable.with_name('bridge.json'), args[0], args[1] if len(args) == 3 else None, args[-1])

if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, TypeError, KeyError):
        sys.exit('remote signing bridge rejected the operation; inspect public request state')
