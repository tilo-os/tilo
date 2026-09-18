#!/usr/bin/python3
"""Service one explicitly authorized signing request locally; no network listener."""
import argparse
import json
from signing_spool import service_local_request

parser = argparse.ArgumentParser(description=__doc__)
for name in ('request-directory', 'policy-path', 'state-directory', 'response-directory', 'signer-path'):
    parser.add_argument('--' + name, required=True)
if __name__ == '__main__':
    try:
        print(json.dumps(service_local_request(**vars(parser.parse_args())), sort_keys=True))
    except (OSError, ValueError, TypeError, KeyError):
        parser.exit(1, 'local signing request rejected; no request-controlled diagnostics emitted\n')
