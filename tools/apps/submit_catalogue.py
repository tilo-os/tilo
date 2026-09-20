#!/usr/bin/env python3
"""Submit already-uploaded app artifacts to the existing moderated catalogue."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import tempfile
import urllib.error
import urllib.parse
import urllib.request

MAX_RESPONSE = 8 * 1024 * 1024


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError('catalogue redirects are not accepted')


def request(url, payload=None):
    body = None if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(url, data=body, headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.build_opener(NoRedirect).open(req, timeout=60) as response:
            data = response.read(MAX_RESPONSE + 1)
            if len(data) > MAX_RESPONSE:
                raise ValueError('catalogue response exceeds size limit')
            return response.status, json.loads(data)
    except urllib.error.HTTPError as error:
        if error.code == 409:
            return 409, None
        raise


def public_https(value):
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme != 'https' or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError('a public HTTPS endpoint without credentials, query or fragment is required')
    if parsed.hostname.endswith('.svc.cluster.local') or parsed.hostname in ('localhost', '127.0.0.1', '::1'):
        raise ValueError('a public HTTPS endpoint is required')
    return value.rstrip('/')


def artifact_plan(release, public_base, version):
    public_base = public_https(public_base)
    if not re.fullmatch(r'[A-Za-z0-9._+-]+', version):
        raise ValueError('invalid immutable upload version')
    artifacts = []
    for index in sorted(release.glob('tilo-apps-*/index.json')):
        for app in json.loads(index.read_text())['apps']:
            name, filename = app['name'], app['file']
            if not re.fullmatch(r'[a-z0-9-]+', name) or Path(filename).name != filename or '\\' in filename:
                raise ValueError('invalid artifact name')
            file = index.parent / 'qpkg' / filename
            if not file.is_file():
                raise ValueError(f'missing artifact: {file}')
            url = f'{public_base}/tilo-app-{name}/{version}/{urllib.parse.quote(filename, safe="")}'
            artifacts.append({'file': str(file.resolve()), 'url': url})
    if not artifacts:
        raise ValueError('no uploaded app artifacts found')
    return artifacts


def submit(args, transport=request):
    endpoint = public_https(args.registry) + '/entries'
    artifacts = artifact_plan(args.release, args.public_base, args.version)
    accepted = 0
    with tempfile.TemporaryDirectory(prefix='tilo-catalogue-') as directory:
        root = Path(directory)
        plan = root / 'artifacts.json'
        snapshot_file = root / 'snapshot.json'
        plan.write_text(json.dumps(artifacts))
        checkpoint = None
        for attempt in range(3):
            status, snapshot = transport(endpoint)
            if status != 200:
                raise ValueError(f'catalogue fetch returned {status}')
            snapshot_file.write_text(json.dumps(snapshot))
            # The existing Rust verifier authenticates root, expiry, publisher
            # signatures and qpkg bytes before any request leaves this process.
            command = [str(args.pkg_binary), 'catalogue-entries', '--snapshot', str(snapshot_file),
                       '--owner-key', args.owner_key, '--artifacts', str(plan), '--identity', str(args.identity)]
            result = subprocess.run(command, check=True, text=True, stdout=subprocess.PIPE)
            requests = json.loads(result.stdout)
            current = (snapshot['sequence'], snapshot)
            if checkpoint is not None and (current[0] < checkpoint[0] or
                                          (current[0] == checkpoint[0] and current[1] != checkpoint[1])):
                raise ValueError('catalogue rolled back or equivocated during submission')
            checkpoint = current
            conflict = False
            for change in requests:
                status, response = transport(endpoint, change)
                if status == 409:
                    conflict = True
                    break
                if status != 202 or response.get('status') != 'pending_review' or response.get('id') != change['entry']['package_id']:
                    raise ValueError(f'catalogue submission returned unexpected response: {status}')
                accepted += 1
            if not conflict:
                return {'status': 'queued', 'accepted_requests': accepted}
        raise ValueError('catalogue changed during all three attempts; retry after pending promotion')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('registry', 'owner-key', 'public-base', 'version'):
        parser.add_argument('--' + name, required=True)
    for name in ('release', 'pkg-binary', 'identity'):
        parser.add_argument('--' + name, type=Path, required=True)
    print(json.dumps(submit(parser.parse_args())))


if __name__ == '__main__':
    main()
