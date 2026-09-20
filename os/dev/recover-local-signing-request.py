#!/usr/bin/python3
"""Retry one exact interrupted local signing reservation once."""
import argparse
import json
from signing_spool import recover_local_request

parser = argparse.ArgumentParser(description=__doc__)
for name in ('reservation-directory', 'response-directory', 'signer-path', 'request-id',
             'operation', 'input-sha256', 'verification-sha256'):
    parser.add_argument('--' + name, required=True)
if __name__ == '__main__':
    try:
        print(json.dumps(recover_local_request(**vars(parser.parse_args())), sort_keys=True))
    except (OSError, ValueError, TypeError, KeyError):
        parser.exit(1, 'local signing recovery rejected; no request-controlled diagnostics emitted\n')
