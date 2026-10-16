#!/usr/bin/python3
"""Finalize retained development rootfs artifacts without application compilation.

Eight resumable immutable checkpoints. Interrupted signing is never retried
implicitly. Publication remains unsigned; registry signing, disk installation and boot are separate gates.
"""
import argparse
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import selectors
import shutil
import signal
import subprocess
import sys
import time
import tempfile
import hardware_profile
import installer_overlay
import signing_spool as spool


class CommandFailure(ValueError):
    """Bounded command failure details for a public, sanitized receipt."""

    def __init__(self, reason, arguments, output, exit_code=None):
        super().__init__(reason)
        self.reason = reason
        self.arguments = tuple(arguments)
        self.output = bytes(output)
        self.exit_code = exit_code


def sanitized_tail(output, limit=4096):
    text = output[-limit * 4:].decode('utf-8', errors='replace')
    text = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', text)
    text = re.sub(r'(?i)([a-z][a-z0-9+.-]*://)[^\s/@:]+:[^\s/@]+@', r'\1***:***@', text)
    text = re.sub(r'(?i)\b(authorization|password|passwd|token|secret|credential|auth)\b(\s*[:=]\s*)([^\s,;]+)',
                  r'\1\2***', text)
    text = ''.join(character if character in '\n\t' or character.isprintable() else '\ufffd' for character in text)
    return text[-limit:]

STAGES = ('rebind', 'loader', 'provisional', 'composefs', 'reseal', 'prediction', 'verified', 'published')
FIELDS = {'schema', 'source_revision', 'source_dir', 'helpers_revision', 'receipt', 'receipt_sha256',
          'public_bundle', 'manifest_sha256', 'target', 'new_public_bundle', 'new_manifest_sha256',
          'new_target', 'new_owner', 'architecture', 'generation', 'source_date_epoch', 'base_ref',
          'rpms_ref', 'kernel_ref', 'signer', 'systemd_version', 'minimum_free_bytes',
          'stage_timeout_seconds', 'signing_response_timeout_seconds',
          'prediction_stage_timeout_seconds', 'publish_repository'}


def validate_request(request):
    if set(request) not in (FIELDS, FIELDS | {'boot_profile'}) or request['schema'] != 1:
        raise ValueError('finalization request fields differ from schema1')
    if request.get('boot_profile', 'installed-target') not in ('installed-target', 'installer-media'):
        raise ValueError('unsupported finalization boot profile')
    for name in ('source_revision', 'helpers_revision'):
        if not re.fullmatch(r'[0-9a-f]{40}', request[name]):
            raise ValueError('committed source and helper revisions required')
    for name in ('receipt_sha256', 'manifest_sha256', 'new_manifest_sha256', 'new_owner'):
        if not re.fullmatch(r'[0-9a-f]{64}', request[name]):
            raise ValueError('invalid independently pinned public identity')
    for name in ('base_ref', 'rpms_ref', 'kernel_ref'):
        if not re.fullmatch(r'[a-z0-9][a-z0-9./:_-]*@sha256:[0-9a-f]{64}', request[name]):
            raise ValueError('immutable build input reference required')
    for name in ('source_dir', 'receipt', 'public_bundle', 'new_public_bundle', 'signer'):
        path = Path(request[name])
        if not path.is_absolute() or '..' in path.parts or path.resolve() != path:
            raise ValueError('canonical absolute finalization paths required')
    if request['architecture'] not in ('aarch64', 'x86_64'):
        raise ValueError('invalid native architecture')
    for name in ('target', 'new_target'):
        if not isinstance(request[name], str) or not 2 <= len(request[name]) <= 513:
            raise ValueError('invalid intended target reference')
        # Match the installed origin contract before launching a worker: a
        # syntactically valid tag can still name the wrong native architecture.
        try:
            hardware_profile.development_target(request[name], request['architecture'])
        except ValueError as error:
            raise ValueError(f'invalid {name}: {error}') from error
    if (not re.fullmatch(r'registry\.tilo\.so/tilo/tilo-development', request['publish_repository'])
            or not re.fullmatch(re.escape(request['publish_repository']) + r':[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}', request['new_target'])):
        raise ValueError('publication must use the bound development repository with a separate staging tag')
    bounds = {'generation': (1, 2**64 - 1), 'source_date_epoch': (0, 253402300799),
              'minimum_free_bytes': (1024**3, 1024**4), 'stage_timeout_seconds': (60, 3600),
              'signing_response_timeout_seconds': (60, 1800),
              # Leave fifteen minutes inside the workflow's 155-minute step
              # deadline for launcher/export cleanup and failure evidence.
              'prediction_stage_timeout_seconds': (300, 8400)}
    for name, (low, high) in bounds.items():
        if type(request[name]) is not int or not low <= request[name] <= high:
            raise ValueError('invalid finalization execution bound')
    if request['prediction_stage_timeout_seconds'] <= (
            request['signing_response_timeout_seconds'] + request['stage_timeout_seconds']):
        raise ValueError('prediction deadline must exceed signing response and non-signing command budgets')
    if not re.fullmatch(r'259(?:[.][0-9]+)?(?:[-+][A-Za-z0-9_.+-]+)?', request['systemd_version']):
        raise ValueError('independently pinned systemd259 version required')
    return request


def run_bounded(arguments, environment, timeout):
    process = subprocess.Popen(arguments, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               env=environment, start_new_session=True)
    deadline, data = time.monotonic() + timeout, bytearray()
    completed = False
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not selector.select(remaining):
                    raise CommandFailure('finalization stage exceeded deadline', arguments, data)
                chunk = process.stdout.read1(65536)
                if not chunk:
                    break
                data.extend(chunk)
                if len(data) > 16 * 1024**2:
                    raise CommandFailure('finalization stage output exceeded bound', arguments, data)
        exit_code = process.wait(timeout=max(0.01, deadline - time.monotonic()))
        if exit_code:
            raise CommandFailure('finalization command failed; checkpoint not accepted', arguments, data, exit_code)
        completed = True
        return bytes(data)
    finally:
        # A leader can exit while a descendant still holds stdout. Kill the
        # owned process group on every failure, even if the leader is gone.
        if not completed:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        process.wait()
        process.stdout.close()


def image_id(value):
    if not isinstance(value, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', value):
        raise ValueError('stage did not return an immutable local image ID')
    return value


def verify_registry_bytes(manifest_raw, config_raw, registry_manifest_digest, manifest_digest,
                          config_digest, architecture, revision):
    # Skopeo may reserialize both inspection outputs. Bind the image through
    # the registry-reported manifest digest and its config descriptor, then
    # validate the decoded config properties instead of hashing presentation
    # bytes as though they were raw registry blobs.
    if image_id(registry_manifest_digest) != manifest_digest:
        raise ValueError('published registry manifest differs from immutable digest')
    manifest = json.loads(manifest_raw, object_pairs_hook=spool.unique)
    config = json.loads(config_raw, object_pairs_hook=spool.unique)
    if (manifest.get('schemaVersion') != 2 or manifest.get('config', {}).get('digest') != config_digest
            or config.get('os') != 'linux' or config.get('architecture') != {'aarch64': 'arm64', 'x86_64': 'amd64'}[architecture]
            or (config.get('config', {}).get('Labels') or {}).get('org.opencontainers.image.revision') != revision):
        raise ValueError('published image configuration identity differs')


def checkpoint_binding(request, request_sha256):
    """Stable identity that may cross workers; deliberately excludes pod/job IDs."""
    return {'request_sha256': request_sha256, 'source_revision': request['source_revision'],
            'helpers_revision': request['helpers_revision'], 'architecture': request['architecture'],
            'new_target': request['new_target'], 'new_manifest_sha256': request['new_manifest_sha256']}


def build_final_command(request, signed_id, iidfile):
    # The exact source recipe remains authoritative. Override its rootfs stage
    # with the signed retained artifact; no builder/rootfs compilation executes.
    command = ['podman', '--remote=false', 'build', '--target', 'final', '--skip-unused-stages=true',
               '--pull=never', '--network=none', '--source-date-epoch', str(request['source_date_epoch']),
               '--rewrite-timestamp', '--iidfile', str(iidfile),
               '--build-context', 'rootfs=container-image://' + image_id(signed_id)]
    for key, value in {'BASE_REF': request['base_ref'], 'RPMS_REF': request['rpms_ref'],
            'KERNEL_RPMS_REF': request['kernel_ref'],
            'TILO_WINDOWS_ARCH': request['architecture'], 'TILO_WITH_BROWSER': '1',
            'TILO_CFS_DIGEST': '0' * 128, 'TILO_SEAL_GENERATION': '1'}.items():
        command += ['--build-arg', key + '=' + value]
    return command + ['-f', str(Path(request['source_dir']) / 'os/Containerfile'), request['source_dir']]


class Driver:
    def __init__(self, request, helpers, evidence, runner=run_bounded):
        self.request = validate_request(request)
        self.helpers = Path(helpers)
        self.dev = self.helpers / 'os/dev'
        self.evidence = Path(evidence)
        self.runner = runner
        self.binding = spool.digest(spool.canonical(request))
        self.environment = {key: os.environ[key] for key in ('PATH', 'HOME', 'REGISTRY_AUTH_FILE',
                            'XDG_RUNTIME_DIR', 'DOCKER_CONFIG') if key in os.environ}
        self.environment.update(PATH='/usr/local/bin:/usr/bin:/bin', LANG='C.UTF-8',
                                SOURCE_DATE_EPOCH=str(request['source_date_epoch']))
        self.receipts = []
        self.restored_stage = None

    def command(self, arguments, timeout=None):
        if timeout is None:
            timeout = self.request['stage_timeout_seconds']
        arguments = [str(a) for a in arguments]
        try:
            return self.runner(arguments, self.environment, timeout)
        except CommandFailure as error:
            receipt = {'schema': 1, 'request_sha256': self.binding,
                       'reason': error.reason, 'executable': Path(error.arguments[0]).name,
                       'argument_count': len(error.arguments),
                       'arguments_sha256': spool.digest(spool.canonical(list(error.arguments))),
                       'exit_code': error.exit_code, 'output_sha256': spool.digest(error.output),
                       'output_tail': sanitized_tail(error.output)}
            try:
                status = spool.document(self.evidence / 'status.json')
                if status.get('request_sha256') == self.binding and isinstance(status.get('stage'), str):
                    receipt['stage'] = status['stage']
                spool.publish(self.evidence / 'failure.json', spool.canonical(receipt))
            except (OSError, ValueError, TypeError, KeyError):
                pass
            raise

    def helper(self, name, arguments):
        raw = self.command(['/usr/bin/python3', self.dev / name, *arguments])
        return json.loads(raw, object_pairs_hook=spool.unique)

    def inspect(self, identifier):
        info = json.loads(self.command(['podman', '--remote=false', 'image', 'inspect', image_id(identifier)]))
        if (len(info) != 1 or 'sha256:' + info[0].get('Id', '').removeprefix('sha256:') != identifier
                or info[0].get('Os') != 'linux' or info[0].get('Architecture') != {'aarch64': 'arm64', 'x86_64': 'amd64'}[self.request['architecture']]
                or (info[0].get('Labels') or {}).get('org.opencontainers.image.revision') != self.request['source_revision']):
            raise ValueError('checkpoint image identity or revision differs')

    def current_context(self):
        """Bind this attempt without making its ephemeral identity part of the checkpoint."""
        context = spool.document(spool.signer.trusted(Path(self.request['signer']).with_name('bridge.json')))['context']
        spool.context(context)
        expected = {'revision': self.request['source_revision'], 'architecture': self.request['architecture'],
                    'manifest_sha256': self.request['new_manifest_sha256'], 'target': self.request['new_target']}
        if any(context.get(key) != value for key, value in expected.items()):
            raise ValueError('current signing context differs from checkpoint request')
        return {'sha256': spool.context(context), **expected}

    def registry_bytes(self, reference):
        digest = image_id(self.command(
            ['skopeo', 'inspect', '--tls-verify=true', '--format={{.Digest}}', 'docker://' + reference]
        ).decode().strip())
        manifest = self.command(['skopeo', 'inspect', '--tls-verify=true', '--raw', 'docker://' + reference])
        config = self.command(['skopeo', 'inspect', '--tls-verify=true', '--config', 'docker://' + reference])
        return manifest, config, digest

    def publish_checkpoint(self, image, stage):
        if stage not in (2, 5):
            raise ValueError('unsupported registry checkpoint stage')
        tag = self.request['publish_repository'] + f':finalization-{self.binding}-stage{stage}'
        digestfile = self.evidence / f'.stage{stage}-published.digest'
        self.command(['podman', '--remote=false', 'push', '--tls-verify=true', '--digestfile', digestfile,
                      image_id(image), 'docker://' + tag])
        manifest_digest = image_id(spool.snapshot(digestfile, 128).decode().strip())
        reference = self.request['publish_repository'] + '@' + manifest_digest
        manifest, config, registry_digest = self.registry_bytes(reference)
        verify_registry_bytes(manifest, config, registry_digest, manifest_digest, image, self.request['architecture'],
                              self.request['source_revision'])
        # Retain producer context as provenance, but do not require a later
        # explicit attempt to reuse the same ephemeral pod/job identity.
        return {'checkpoint_reference': reference, 'checkpoint_manifest_digest': manifest_digest,
                'checkpoint_config_digest': image, 'checkpoint_binding': checkpoint_binding(self.request, self.binding),
                'checkpoint_stage': stage, 'producer_context_sha256': self.current_context()['sha256']}

    def restore_checkpoint(self):
        """Restore one complete immutable stage-2 or stage-5 chain."""
        available = [(self.evidence / f'{number:02d}-{STAGES[number - 1]}.json').exists()
                     for number in range(1, 6)]
        if not any(available):
            return False
        if available == [True, True, False, False, False]:
            stage = 2
        elif available == [True] * 5:
            stage = 5
        else:
            raise ValueError('registry checkpoint receipt set is incomplete')
        paths = [self.evidence / f'{number:02d}-{STAGES[number - 1]}.json'
                 for number in range(1, stage + 1)]
        receipts, parent_sha = [], None
        for number, path in enumerate(paths, 1):
            receipt = spool.document(path)
            if (receipt.get('schema') != 1 or receipt.get('request_sha256') != self.binding
                    or receipt.get('stage') != STAGES[number - 1]
                    or receipt.get('parent_receipt_sha256') != parent_sha):
                raise ValueError(f'stage-{stage} checkpoint receipt chain differs')
            receipts.append(receipt)
            parent_sha = spool.digest(spool.canonical(receipt))
        checkpoint = receipts[-1]
        manifest_digest = image_id(checkpoint.get('checkpoint_manifest_digest'))
        config_digest = image_id(checkpoint.get('checkpoint_config_digest'))
        reference = checkpoint.get('checkpoint_reference')
        if (checkpoint.get('image_id') != config_digest
                or reference != self.request['publish_repository'] + '@' + manifest_digest
                or checkpoint.get('checkpoint_binding') != checkpoint_binding(self.request, self.binding)
                or checkpoint.get('checkpoint_stage', 5 if stage == 5 else None) != stage):
            raise ValueError(f'stage-{stage} checkpoint stable binding differs')
        if not re.fullmatch(r'[0-9a-f]{64}', checkpoint.get('producer_context_sha256', '')):
            raise ValueError(f'stage-{stage} checkpoint producer context identity invalid')
        # current_context validates this new attempt against the stable request.
        # Its digest is intentionally allowed to differ from the producer.
        self.current_context()
        manifest, config, registry_digest = self.registry_bytes(reference)
        verify_registry_bytes(manifest, config, registry_digest, manifest_digest, config_digest, self.request['architecture'],
                              self.request['source_revision'])
        self.command(['podman', '--remote=false', 'pull', '--tls-verify=true', reference])
        self.inspect(config_digest)
        self.restored_stage = stage
        return True

    def preflight(self):
        q = self.request
        if os.getuid() != 0 or platform.machine() != q['architecture']:
            raise ValueError('already-root native finalization worker required')
        for key in ('CONTAINER_HOST', 'CONTAINER_CONNECTION', 'CONTAINER_SSHKEY', 'DOCKER_HOST',
                    'CONTAINERS_STORAGE_CONF', 'CONTAINERS_CONF', 'CONTAINERS_CONF_OVERRIDE', 'STORAGE_DRIVER', 'STORAGE_OPTS'):
            if os.environ.get(key):
                raise ValueError('remote or custom container store forbidden')
        for root, marker, revision in ((Path(q['source_dir']), '.finalization-source.json', q['source_revision']),
                                       (self.helpers, '.finalization-helpers.json', q['helpers_revision'])):
            if spool.document(spool.signer.trusted(root / marker)) != {'revision': revision}:
                raise ValueError('launcher source/helper checkout attestation differs')
        receipt = spool.snapshot(q['receipt'], 65536)
        if spool.digest(receipt) != q['receipt_sha256']:
            raise ValueError('retained raw receipt bytes differ')
        spool.signer.trusted(Path(q['signer']))
        if not os.access(q['signer'], os.X_OK):
            raise ValueError('external signing adapter is not executable')
        self.capacity()
        # Pull-only public inputs for the final target; never invoke Cargo/DNF
        # on the host, or an earlier Containerfile target as a fallback.
        self.command(['podman', '--remote=false', 'pull', '--tls-verify=true', q['base_ref']])
        self.command(['podman', '--remote=false', 'pull', '--tls-verify=true', q['rpms_ref']])

    def capacity(self):
        info = json.loads(self.command(['podman', '--remote=false', 'info', '--format=json']))
        graph = Path(info['store']['graphRoot'])
        if not graph.is_absolute():
            raise ValueError('container storage path is not absolute')
        required = self.request['minimum_free_bytes']
        if shutil.disk_usage(graph).free < required or shutil.disk_usage(self.evidence).free < required:
            raise ValueError('admitted finalization scratch reserve unavailable')

    def status(self, value):
        value = {'schema': 1, 'request_sha256': self.binding, **value}
        temporary = self.evidence / ('.status-' + str(os.getpid()))
        spool.write_new(temporary, spool.canonical(value))
        os.replace(temporary, self.evidence / 'status.json')
        spool.sync_directory(self.evidence)

    def stage(self, number, operation):
        name = STAGES[number - 1]
        path = self.evidence / f'{number:02d}-{name}.json'
        parent_sha = spool.digest(spool.canonical(self.receipts[-1])) if self.receipts else None
        if path.exists():
            receipt = spool.document(path)
            if (receipt.get('request_sha256') != self.binding or receipt.get('stage') != name
                    or receipt.get('parent_receipt_sha256') != parent_sha):
                raise ValueError('completed checkpoint belongs to another input chain')
            if receipt.get('image_id') and (number == self.restored_stage or self.restored_stage is None):
                self.inspect(receipt['image_id'])
            self.receipts.append(receipt)
            return receipt
        marker = self.evidence / f'.started-{number:02d}.json'
        if marker.exists():
            raise ValueError('stage previously started without accepted checkpoint; resolve pending signing before explicit new job')
        self.capacity()
        spool.publish(marker, spool.canonical({'request_sha256': self.binding, 'stage': name, 'pid': os.getpid()}))
        self.status({'stage': name, 'state': 'running', 'automatic_retry': False})
        result = operation()
        if result.get('image_id'):
            self.inspect(result['image_id'])
        receipt = {'schema': 1, 'request_sha256': self.binding, 'stage': name,
                   'parent_receipt_sha256': parent_sha, **result}
        spool.publish(path, spool.canonical(receipt))
        self.receipts.append(receipt)
        self.status({'stage': name, 'state': 'checkpoint-complete', 'automatic_retry': False})
        return receipt

    def execute(self):
        q = self.request
        self.preflight()
        self.restore_checkpoint()
        base = ['--receipt', q['receipt'], '--public-bundle', q['public_bundle'], '--revision', q['source_revision'],
                '--architecture', q['architecture'], '--manifest-sha256', q['manifest_sha256'], '--target', q['target'],
                '--generation', str(q['generation']), '--source-date-epoch', str(q['source_date_epoch'])]
        def rebound():
            receipt = self.helper('rebind-development-rootfs.py', base + ['--new-public-bundle', q['new_public_bundle'],
                '--new-manifest-sha256', q['new_manifest_sha256'], '--new-target', q['new_target'], '--new-owner', q['new_owner']])
            return {'image_id': image_id(receipt['rootfs_id']), 'evidence': receipt}
        rebound_id = self.stage(1, rebound)['image_id']
        def loader():
            receipt = self.helper('sign-development-rootfs.py', ['--rootfs-id', rebound_id, '--architecture', q['architecture'],
                '--revision', q['source_revision'], '--public-bundle', q['new_public_bundle'], '--manifest-sha256', q['new_manifest_sha256'],
                '--target', q['new_target'], '--source-date-epoch', str(q['source_date_epoch']), '--signer', q['signer']])
            identifier = image_id(receipt['signed_rootfs_id'])
            return {'image_id': identifier, 'evidence': receipt, **self.publish_checkpoint(identifier, 2)}
        signed_id = self.stage(2, loader)['image_id']
        def provisional():
            iid = self.evidence / '.provisional.iid'
            self.command(build_final_command(q, signed_id, iid))
            provisional_id = image_id(spool.snapshot(iid, 128).decode().strip())
            overlay = None
            if q.get('boot_profile') == 'installer-media':
                with tempfile.TemporaryDirectory(prefix='tilo-installer-overlay-', dir='/var/lib') as temporary:
                    context = Path(temporary) / 'context'
                    files = installer_overlay.stage(q['source_dir'], self.helpers, context)
                    media_iid = Path(temporary) / 'media.iid'
                    self.command(installer_overlay.command(context, provisional_id, q['source_date_epoch'], media_iid))
                    overlay = {'base_provisional_image_id': provisional_id, 'files_sha256': files,
                               'helpers_revision': q['helpers_revision'], 'source_revision': q['source_revision'],
                               'application_compilation': False}
                    provisional_id = image_id(spool.snapshot(media_iid, 128).decode().strip())
            return {'image_id': provisional_id, 'installer_overlay': overlay,
                    'source_recipe_sha256': spool.digest(spool.snapshot(Path(q['source_dir']) / 'os/Containerfile', 1024**2)),
                    'application_compilation': False}
        provisional_id = self.stage(3, provisional)['image_id']
        def compute(identifier):
            value = self.helper('compute-development-composefs.py', ['--image-id', identifier, '--architecture', q['architecture']])
            if not re.fullmatch(r'[0-9a-f]{128}', value['composefs_digest']) or value['composefs_digest'] == '0' * 128:
                raise ValueError('computed composefs digest invalid')
            return {'image_id': identifier, 'composefs_digest': value['composefs_digest'], 'evidence': value}
        seal = self.stage(4, lambda: compute(provisional_id))['composefs_digest']
        def reseal():
            iid = self.evidence / '.resealed.iid'
            self.command(['/bin/bash', self.dev / 'reseal-image.sh', provisional_id, seal,
                          '--iidfile', iid, '--source-date-epoch', str(q['source_date_epoch'])])
            identifier = image_id(spool.snapshot(iid, 128).decode().strip())
            return {'image_id': identifier, 'composefs_digest': seal, **self.publish_checkpoint(identifier, 5)}
        resealed_id = self.stage(5, reseal)['image_id']
        def prediction():
            deadline = time.monotonic() + q['prediction_stage_timeout_seconds']
            def command(arguments, *, aggregate=False):
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ValueError('prediction stage exceeded aggregate deadline')
                timeout = remaining if aggregate else min(q['stage_timeout_seconds'], remaining)
                return self.command(arguments, timeout=timeout)
            # Existing helper accepts a mutable local alias. Confine it to this
            # request-owned alias; retain only immutable IDs in receipts.
            alias = 'localhost/tilo-finalization-' + self.binding + ':prediction'
            command(['podman', '--remote=false', 'tag', resealed_id, alias])
            sys.path.insert(0, str(self.dev))
            trust = self.module('finalization_trust', 'development-trust.py')
            metadata = trust.validate(Path(q['new_public_bundle']), q['new_manifest_sha256'])
            self.environment['TILO_EFI_SIGNING_CERT_SHA256'] = metadata['identities']['efi-signing.crt']
            command(['/bin/bash', self.dev / 'sign-target-prediction.sh', alias, q['signer']], aggregate=True)
            value = json.loads(command(['podman', '--remote=false', 'image', 'inspect', alias]))
            return {'image_id': image_id('sha256:' + value[0]['Id'].removeprefix('sha256:')), 'composefs_digest': seal}
        final_id = self.stage(6, prediction)['image_id']
        def verify():
            digest = compute(final_id)
            if digest['composefs_digest'] != seal:
                raise ValueError('root digest moved after boot-only signing')
            arguments = ['/usr/bin/python3', Path(__file__).with_name('verify-finalized-development-image.py'),
                         '--helpers', self.helpers, '--image-id', final_id, '--target', q['new_target'],
                         '--public-bundle', q['new_public_bundle'], '--manifest-sha256', q['new_manifest_sha256'],
                         '--architecture', q['architecture'], '--generation', str(q['generation']),
                         '--composefs-digest', seal, '--systemd-version', q['systemd_version']]
            arguments += ['--boot-profile', q.get('boot_profile', 'installed-target')]
            proof = json.loads(self.command(arguments), object_pairs_hook=spool.unique)
            return {'image_id': final_id, 'composefs_digest': seal, 'verification': proof,
                    'composefs': digest['evidence'], 'installable': False}
        final = self.stage(7, verify)
        def publish():
            tag = q['publish_repository'] + ':finalization-' + self.binding
            digestfile = self.evidence / '.published.digest'
            self.command(['podman', '--remote=false', 'push', '--tls-verify=true', '--quiet', '--format=oci',
                          '--digestfile', digestfile, final_id, 'docker://' + tag])
            manifest_digest = spool.snapshot(digestfile, 128).decode().strip()
            image_id(manifest_digest)
            reference = q['publish_repository'] + '@' + manifest_digest
            manifest, configuration, registry_digest = self.registry_bytes(reference)
            verify_registry_bytes(manifest, configuration, registry_digest, manifest_digest, final_id,
                                  q['architecture'], q['source_revision'])
            return {'image_id': final_id, 'published_reference': reference, 'manifest_digest': manifest_digest,
                    'config_digest': final_id, 'staging_tag': tag, 'target_update_ref': q['new_target'],
                    'boot_profile': q.get('boot_profile', 'installed-target'),
                    'published_tag_updates_installed_origin': False,
                    'published': True, 'registry_signature_verified': False, 'cosign_signing_required': True, 'installable': False}
        published = self.stage(8, publish)
        result = {'schema': 1, 'request_sha256': self.binding, 'purpose': 'development-finalization-staged',
                  'image_id': final_id, 'composefs_digest': seal, 'verified_checkpoint_sha256': spool.digest(spool.canonical(final)),
                  'published_reference': published['published_reference'], 'manifest_digest': published['manifest_digest'],
                  'config_digest': final_id, 'published': True, 'installable': False, 'publication_required': False,
                  'cosign_signing_required': True, 'disk_and_boot_verification_required': True}
        destination = self.evidence / 'result.json'
        if destination.exists():
            if spool.document(destination) != result:
                raise ValueError('final result changed')
        else:
            spool.publish(destination, spool.canonical(result))
        return result

    def module(self, name, filename):
        spec = importlib.util.spec_from_file_location(name, self.dev / filename)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--request', required=True, type=Path)
    parser.add_argument('--helpers', required=True, type=Path)
    parser.add_argument('--evidence', required=True, type=Path)
    args = parser.parse_args()
    args.evidence.mkdir(mode=0o700, parents=False, exist_ok=True)
    spool.private_directory(args.evidence)
    descriptor = os.open(args.evidence / '.lock', os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    driver = None
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        driver = Driver(spool.document(args.request), args.helpers, args.evidence)
        print(json.dumps(driver.execute(), sort_keys=True))
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError):
        if driver:
            driver.status({'state': 'failed-or-interrupted', 'automatic_retry': False,
                           'action': 'inspect live process and pending signing request before explicit new job'})
        parser.exit(1, 'finalization did not complete; inspect public checkpoint status and owned live handles\n')
    finally:
        os.close(descriptor)

if __name__ == '__main__':
    main()
