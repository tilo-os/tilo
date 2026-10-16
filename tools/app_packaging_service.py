#!/usr/bin/env python3
"""One platform-owned packaging invocation; read-only unless --apply is supplied."""
import argparse
from contextlib import contextmanager
import json
from pathlib import Path
import re
import time
import urllib.error
import urllib.request

import app_packaging_coordinator as core
import app_packaging_product as product
from accept_app_artifacts import FinalArtifactAcceptance
from assemble_verified_apps import MAX_BYTES, ReadOnlyAPI, download_destination
import app_channel_manifest as channel
import canonical_verifier_bundle as bundle

ORIGIN = 'https://code.tilo.so'
PREFIX = 'repos/tilo/tilo'
DISPATCH = PREFIX + '/actions/workflows/release-apps.yml/dispatches'
MAX_JSON = 2 * 1024**2


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def allowed(path, method):
    if method == 'POST':
        return path == DISPATCH
    if method != 'GET':
        return False
    return path in ('user', PREFIX, PREFIX + '/branches/main') or bool(re.fullmatch(
        r'repos/tilo/tilo/(?:contents/[A-Za-z0-9_./-]*\?ref=[0-9a-f]{40}'
        r'|actions/runs/[1-9][0-9]*(?:/jobs|/artifacts\?page=1&limit=50)?'
        r'|actions/(?:jobs/[1-9][0-9]*/logs|artifacts/[1-9][0-9]*/zip))', path)) and '..' not in path.split('?', 1)[0].split('/')


class Transport:
    """Fixed-origin authenticated transport with bounded reads and no redirects."""
    def __init__(self, token, read_only=False):
        product.require(isinstance(token, str) and token and not any(c.isspace() for c in token), 'invalid token file')
        self.token = token
        self.read_only = read_only
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def request(self, path, data=None, maximum=MAX_JSON, output=None):
        method = 'GET' if data is None else 'POST'
        product.require(not self.read_only or method == 'GET', 'reconciliation transport is read-only')
        product.require(allowed(path, method), 'API route or method is not permitted')
        product.require(type(maximum) is int and 0 < maximum <= MAX_BYTES, 'invalid response bound')
        body = core.canonical(data).encode() if data is not None else None
        product.require(body is None or len(body) <= MAX_JSON, 'dispatch request exceeds bound')
        request = urllib.request.Request(ORIGIN + '/api/v1/' + path, data=body, method=method,
            headers={'Authorization': 'Bearer ' + self.token, 'Content-Type': 'application/json', 'Cache-Control': 'no-cache'})
        # The socket timeout bounds stalled reads. The deadline also bounds a
        # peer that keeps making partial progress. No response/error bodies log.
        deadline = time.monotonic() + (300 if output else 30)
        # Redact remote failures only. Local destination errors must retain
        # their filesystem type and path rather than masquerade as API errors.
        try:
            response = self.opener.open(request, timeout=30)
        except (urllib.error.URLError, OSError):
            raise ValueError('authenticated API request failed') from None
        with response:
            product.require(response.geturl() == request.full_url, 'API redirect rejected')
            length = response.headers.get('Content-Length')
            product.require(length is None or (length.isdigit() and int(length) <= maximum), 'API response exceeds bound')
            chunks, size = [], 0
            with download_destination(output) as target:
                while True:
                    product.require(time.monotonic() < deadline, 'API request deadline exceeded')
                    try:
                        data = response.read1(min(65536, maximum + 1 - size))
                    except (urllib.error.URLError, OSError):
                        raise ValueError('authenticated API request failed') from None
                    if not data:
                        break
                    size += len(data)
                    product.require(size <= maximum, 'API response exceeds bound')
                    if target:
                        target.write(data)
                    else:
                        chunks.append(data)
                product.require(length is None or size == int(length), 'API response truncated')
                return None if target else b''.join(chunks)


class ManagedProduct(product.ProductForgejo):
    def __init__(self, policy, transport):
        super().__init__(policy)
        self.transport = transport

    def call(self, path, data=None):
        return channel.read_json(self.transport.request(path, data))

    def raw_api(self, path, maximum):
        return self.transport.request(path, maximum=maximum)


class ManagedReadOnly(ReadOnlyAPI):
    def __init__(self, transport):
        self.transport = transport

    def get(self, path, output=None):
        product.require(path.startswith('/actions/'), 'artifact API path is not permitted')
        raw = self.transport.request(PREFIX + path, maximum=MAX_BYTES if output else MAX_JSON, output=output)
        return None if output else channel.read_json(raw)


def read_bounded(path, maximum=MAX_JSON):
    with Path(path).open('rb') as source:
        raw = source.read(maximum + 1)
    product.require(len(raw) <= maximum, 'platform file exceeds limit')
    return raw


def validate_policy(policy, final_bytes):
    product.require(policy.get('enabled') is True and policy.get('qualification_only') is True,
                    'platform qualification is disabled')
    product.require(type(policy.get('service_user_id')) is int and policy['service_user_id'] > 0,
                    'dedicated service identity required')
    product.require(policy.get('bundle_compatibility_approved') is True, 'native verifier compatibility has not been approved')
    final_sha = product.digest(policy.get('final_receipt_policy_sha256'))
    final = channel.pinned_json(final_bytes, final_sha)
    architecture = bundle.native_arch()
    bundle.resolve_policy(final, architecture)
    descriptor = final['canonical_verifier_bundles'][architecture]
    builder = policy.get('builder_image')
    bundle.identity(builder, descriptor['source_sha'], architecture)
    product.require(descriptor['compatible_builder'] == builder, 'verifier builder compatibility mismatch')
    product.require(type(descriptor['bytes']) is int and 0 < descriptor['bytes'] <= bundle.MAX_BYTES,
                    'invalid verifier bundle size')
    product.require(descriptor['url'] == f"{ORIGIN}/api/packages/tilo/generic/tilo-canonical-verifiers/{descriptor['sha256']}/{architecture}.tar.gz",
                    'verifier bundle must use fixed trusted origin and content identity')
    return final_sha


@contextmanager
def invocation_lock(state_path):
    # Linux deployment only. Use the same lock for CronJobs and manual Jobs.
    import fcntl
    with Path(str(state_path) + '.lock').open('a+b') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError('packaging invocation already active') from None
        try:
            yield
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)


def invoke(state_path, policy_path, final_path, token_path, apply=False, reconcile_run_id=None):
    if reconcile_run_id is not None:
        product.require(type(reconcile_run_id) is int and reconcile_run_id > 0, 'invalid reconciliation run ID')
        product.require(apply, 'reconciliation requires --apply')
    if not apply:
        store = core.Store(state_path)
        try:
            state = store.read()
            return {'dry_run': True, 'next_action': core.next_action(state), 'state': state}
        finally:
            if store.db:
                store.db.close()
    with invocation_lock(state_path):
        if reconcile_run_id is not None:
            existing = core.Store(state_path)
            try:
                active = existing.read()['active']
                product.require(active is not None, 'no active intent to reconcile')
                product.require(active.get('run_id') in (None, reconcile_run_id),
                                'run ID conflicts with active intent')
            finally:
                if existing.db:
                    existing.db.close()
        policy_bytes = read_bounded(policy_path)
        policy = channel.read_json(policy_bytes)
        final_bytes = read_bounded(final_path)
        final_sha = validate_policy(policy, final_bytes)
        token = read_bounded(token_path, 4096).decode('ascii').strip()
        transport = Transport(token, read_only=reconcile_run_id is not None)
        api = ManagedProduct(policy, transport)
        user = api.call('user')
        product.require(type(user.get('id')) is int and user['id'] == policy['service_user_id']
                        and user.get('is_admin') is False, 'unexpected or administrative service identity')
        repo = api.call(PREFIX)
        product.require(repo.get('id') == 23 and repo.get('full_name') == 'tilo/tilo', 'repository identity mismatch')
        if reconcile_run_id is None:
            branch = api.call(PREFIX + '/branches/main')
            product.require(branch.get('name') == 'main', 'main branch identity mismatch')
            desired = product.validate_desired({'source_sha': branch['commit']['id'],
                'builder_image': policy['builder_image'], 'collection': policy['collection'],
                'architectures': policy['architectures'], 'policy_sha256': api.policy_sha})
        api.verifier = FinalArtifactAcceptance(api, ManagedReadOnly(transport), final_path, final_sha, bundle_token=token)
        # ConfigMap rotations cannot silently authorize work under a mixed pair
        # of files. These platform inputs must remain stable for one invocation.
        def unchanged():
            product.require(read_bounded(policy_path) == policy_bytes and read_bounded(final_path) == final_bytes,
                            'platform policy changed during invocation')
        original_dispatch = api.dispatch
        def dispatch(intent):
            unchanged()
            return original_dispatch(intent)
        api.dispatch = dispatch
        original_validate = api.validate_run
        def validate_run(intent, run):
            unchanged()
            if reconcile_run_id is not None:
                product.require(type(run.get('id')) is int and run['id'] == reconcile_run_id,
                                'API returned a different reconciliation run')
                actor = run.get('trigger_user')
                product.require(isinstance(actor, dict) and type(actor.get('id')) is int
                                and actor['id'] == policy['service_user_id'] and actor.get('is_admin') is False,
                                'run actor is not the dedicated service identity')
            try:
                result = original_validate(intent, run)
            except core.VerificationRequired:
                # Reconciliation may retain this run as awaiting-verification;
                # policy rotation must prevent that state write as well.
                if reconcile_run_id is not None:
                    unchanged()
                raise
            unchanged()
            return result
        api.validate_run = validate_run
        store = core.Store(state_path, writable=True)
        try:
            current = store.read()
            unchanged()
            if reconcile_run_id is not None:
                # Recheck the active source against current trusted files; do not
                # observe main, replace the intent, or enter any dispatch path.
                api.qualify_receipt(current['active']['desired'])
                unchanged()
                product.reconcile(store, api, reconcile_run_id)
            else:
                if current['desired'] != desired:
                    store.observe(current['sequence'] + 1, desired)
                core.tick(store, api)
            unchanged()
            state = store.read()
            return {'dry_run': False, 'next_action': core.next_action(state), 'state': state}
        finally:
            store.db.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('state', 'policy', 'final-policy', 'token-file'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--apply', action='store_true')
    parser.add_argument('--reconcile-run-id', type=int,
                        help='recover an existing intent from this exact run using GET requests only; requires --apply')
    args = parser.parse_args()
    result = invoke(args.state, args.policy, args.final_policy, args.token_file, args.apply, args.reconcile_run_id)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
