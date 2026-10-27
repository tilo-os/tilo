#!/usr/bin/env python3
"""Installed-system image verification, end to end, with the real verifier.

skopeo (containers/image, the library bootc verifies updates with) copies
images from a registry served by this test, under the container policy
tools/signing/release_trust.py generates, with only its paths and the
repository renamed to the test's. The root and operational certificates come
from the ceremony script (tools/signing/release-ca.sh); the transparency log
is a stand-in whose key signs each entry's signed timestamp exactly as Rekor
does. Nothing leaves the machine and no key outlives the test.

It proves the chain (release -> operational certificate -> root) is accepted,
and that an expired or not yet valid certificate, a certificate under another
root, one revoked by an epoch increase, one for another purpose, a log entry
signed by another log, and a signature with no log entry are refused; and
that the key-mode policy accepts the bare key signature and refuses a
certificate entry, which is why release images carry both during the cutover.

Needs skopeo, openssl and python3.
"""
import base64
import contextlib
import hashlib
import http.server
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import threading
import time
import unittest
from unittest import mock

REPO = Path(__file__).resolve().parents[2]
CEREMONY = REPO / 'tools/signing/release-ca.sh'
spec = importlib.util.spec_from_file_location('release_trust', REPO / 'tools/signing/release_trust.py')
release_trust = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release_trust)

DAY = 86400
OCI_MANIFEST = 'application/vnd.oci.image.manifest.v1+json'


def run(*args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, **kwargs).stdout


def digest(data):
    return 'sha256:' + hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


class Registry(http.server.ThreadingHTTPServer):
    """A read-only OCI distribution endpoint over a dict of blobs and tags."""

    def __init__(self):
        self.blobs, self.tags = {}, {}
        super().__init__(('127.0.0.1', 0), RegistryHandler)

    def add(self, data):
        self.blobs[digest(data)] = data
        return digest(data)


class RegistryHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_HEAD(self):
        self.answer(body=False)

    def do_GET(self):
        self.answer(body=True)

    def answer(self, body):
        path = self.path.split('?', 1)[0]
        data, kind = None, 'application/octet-stream'
        if path == '/v2/':
            data = b'{}'
        elif '/manifests/' in path:
            reference = path.rsplit('/manifests/', 1)[1]
            reference = self.server.tags.get((path.split('/manifests/')[0], reference), reference)
            data, kind = self.server.blobs.get(reference), OCI_MANIFEST
        elif '/blobs/' in path:
            data = self.server.blobs.get(path.rsplit('/blobs/', 1)[1])
        if data is None:
            self.send_response(404)
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        self.send_response(200)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Docker-Content-Digest', digest(data))
        self.end_headers()
        if body:
            self.wfile.write(data)


class ReleaseTrustChainNative(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for tool in ('skopeo', 'openssl'):
            if not shutil.which(tool):
                raise RuntimeError(f'{tool} is required')
        cls.tmp = tempfile.TemporaryDirectory(prefix='tilo-release-trust-')
        cls.work = Path(cls.tmp.name)
        cls.registry = Registry()
        threading.Thread(target=cls.registry.serve_forever, daemon=True).start()
        cls.host = f'127.0.0.1:{cls.registry.server_address[1]}'
        cls.repository = f'{cls.host}/tilo/tilo'
        (cls.work / 'registries.d').mkdir()
        (cls.work / 'registries.d/test.yaml').write_text(
            f'docker:\n  {cls.host}/tilo:\n    use-sigstore-attachments: true\n')
        # The trust files as a later revocation leaves them: epoch 2.
        cls.trust_epoch_2 = cls.work / 'trust-epoch-2.env'
        cls.trust_epoch_2.write_text(release_trust.TRUST_ENV.read_text().replace(
            f'TILO_RELEASE_EPOCH={release_trust.epoch()}', 'TILO_RELEASE_EPOCH=2'))
        cls.keys = {name: cls.key(name, curve) for name, curve in (
            ('root', 'P-384'), ('other-root', 'P-384'), ('operational', 'P-256'),
            ('rekor', 'P-256'), ('other-rekor', 'P-256'))}
        for root in ('root', 'other-root'):
            cls.ceremony('init-root', '--key', cls.keys[root], '--out', cls.work / f'{root}.crt')
        cls.certs = {}
        for name, purpose, epoch in (('os-images-1', 'os-images', 1),
                                     ('os-images-2', 'os-images', 2),
                                     ('extensions-1', 'extension-manifests', 1)):
            out = cls.work / f'{name}.crt'
            cls.ceremony('issue', '--root', cls.work / 'root.crt', '--key', cls.keys['root'],
                         '--purpose', purpose, '--public-key', cls.public('operational'),
                         '--epoch', str(epoch), '--days', '30', '--out', out)
            cls.certs[name] = out.read_bytes()
        cls.issued = int(time.time())

    @classmethod
    def tearDownClass(cls):
        cls.registry.shutdown()
        cls.tmp.cleanup()

    @classmethod
    def key(cls, name, curve):
        path = cls.work / f'{name}.key'
        run('openssl', 'genpkey', '-algorithm', 'EC', '-pkeyopt', f'ec_paramgen_curve:{curve}',
            '-out', str(path))
        return path

    @classmethod
    def public(cls, name):
        path = cls.work / f'{name}.pub'
        if not path.exists():
            path.write_bytes(run('openssl', 'pkey', '-in', str(cls.keys[name]), '-pubout'))
        return path

    @classmethod
    def ceremony(cls, *args, trust=None):
        env = dict(os.environ)
        if trust:
            env['TILO_RELEASE_TRUST_ENV'] = str(trust)
        return run('bash', str(CEREMONY), *map(str, args), env=env)

    def sign(self, key, data):
        path = self.work / 'payload'
        path.write_bytes(data)
        return run('openssl', 'dgst', '-sha256', '-sign', str(self.keys[key]), str(path))

    def publish(self, name):
        """A one-file image under the test repository; its manifest digest."""
        layer = io.BytesIO()
        with tarfile.open(fileobj=layer, mode='w:gz') as archive:
            info = tarfile.TarInfo(name)
            content = name.encode()
            info.size = len(content)
            archive.addfile(info, io.BytesIO(content))
        layer = layer.getvalue()
        config = json.dumps({'architecture': 'amd64', 'os': 'linux',
                             'rootfs': {'type': 'layers', 'diff_ids': [
                                 digest(run('gzip', '-dc', input=layer))]}}).encode()
        manifest = json.dumps({
            'schemaVersion': 2, 'mediaType': OCI_MANIFEST,
            'config': {'mediaType': 'application/vnd.oci.image.config.v1+json',
                       'digest': self.registry.add(config), 'size': len(config)},
            'layers': [{'mediaType': 'application/vnd.oci.image.layer.v1.tar+gzip',
                        'digest': self.registry.add(layer), 'size': len(layer)}],
        }).encode()
        return self.registry.add(manifest)

    def attach(self, image, cert=None, rekor='rekor', integrated=None, logged=True):
        """Cosign's signature for IMAGE: by the operational key, carrying CERT
        (else the bare key), with a log entry signed by REKOR at INTEGRATED."""
        payload = json.dumps({'critical': {
            'identity': {'docker-reference': self.repository},
            'image': {'docker-manifest-digest': image},
            'type': 'cosign container image signature'}, 'optional': None}).encode()
        signature = base64.b64encode(self.sign('operational', payload)).decode()
        annotations = {'dev.cosignproject.cosign/signature': signature}
        verifier = self.public('operational').read_bytes()
        if cert:
            annotations['dev.sigstore.cosign/certificate'] = self.certs[cert].decode()
            annotations['dev.sigstore.cosign/chain'] = (self.work / 'root.crt').read_text()
            verifier = self.certs[cert]
        if logged:
            body = base64.b64encode(canonical({
                'apiVersion': '0.0.1', 'kind': 'hashedrekord', 'spec': {
                    'data': {'hash': {'algorithm': 'sha256',
                                      'value': hashlib.sha256(payload).hexdigest()}},
                    'signature': {'content': signature,
                                  'publicKey': {'content': base64.b64encode(verifier).decode()}}}})).decode()
            log_key = run('openssl', 'pkey', '-pubin', '-in', str(self.public(rekor)), '-outform', 'DER')
            entry = {'body': body, 'integratedTime': integrated or self.issued + 60,
                     'logID': hashlib.sha256(log_key).hexdigest(), 'logIndex': 1}
            annotations['dev.sigstore.cosign/bundle'] = json.dumps({
                'SignedEntryTimestamp': base64.b64encode(self.sign(rekor, canonical(entry))).decode(),
                'Payload': entry})
        config = b'{}'
        manifest = json.dumps({
            'schemaVersion': 2, 'mediaType': OCI_MANIFEST,
            'config': {'mediaType': 'application/vnd.oci.image.config.v1+json',
                       'digest': self.registry.add(config), 'size': len(config)},
            'layers': [{'mediaType': 'application/vnd.dev.cosign.simplesigning.v1+json',
                        'digest': self.registry.add(payload), 'size': len(payload),
                        'annotations': annotations}],
        }).encode()
        self.registry.tags[('/v2/tilo/tilo', image.replace(':', '-') + '.sig')] = self.registry.add(manifest)

    def policy(self, chain=True, root='root', trust=None):
        """The generated policy, with the installed paths and the OS repository
        replaced by the test's."""
        with contextlib.ExitStack() as stack:
            if chain:
                stack.enter_context(mock.patch.object(release_trust, 'ROOT', self.work / f'{root}.crt'))
            else:
                stack.enter_context(mock.patch.object(release_trust, 'ROOT', self.work / 'absent'))
            if trust:
                stack.enter_context(mock.patch.object(release_trust, 'TRUST_ENV', trust))
            text = release_trust.render()
        for installed, local in ((release_trust.INSTALLED_ROOT, self.work / f'{root}.crt'),
                                 (release_trust.INSTALLED_REKOR, self.public('rekor')),
                                 (release_trust.INSTALLED_KEY, self.public('operational'))):
            text = text.replace(f'"{installed}"', json.dumps(str(local)))
        document = json.loads(text)
        docker = document['transports']['docker']
        docker[self.repository] = docker.pop('registry.tilo.so/tilo/tilo')
        path = self.work / 'policy.json'
        path.write_text(json.dumps(document))
        return path

    def pull(self, image, policy):
        destination = self.work / 'pulled'
        shutil.rmtree(destination, ignore_errors=True)
        return subprocess.run(
            ['skopeo', 'copy', '--quiet', '--src-tls-verify=false', '--policy', str(policy),
             '--registries.d', str(self.work / 'registries.d'),
             f'docker://{self.repository}@{image}', f'dir:{destination}'],
            capture_output=True, text=True)

    def assert_accepted(self, image, policy):
        result = self.pull(image, policy)
        self.assertEqual(result.returncode, 0, result.stderr)

    def assert_refused(self, image, policy, reason):
        result = self.pull(image, policy)
        self.assertNotEqual(result.returncode, 0, 'the policy accepted the image')
        self.assertIn('rejected', result.stderr)
        self.assertIn(reason, result.stderr)

    def test_a_release_chains_through_its_operational_certificate_to_the_root(self):
        image = self.publish('chained')
        self.attach(image, cert='os-images-1')
        self.assert_accepted(image, self.policy())

    def test_an_expired_certificate_is_refused(self):
        image = self.publish('expired')
        self.attach(image, cert='os-images-1', integrated=self.issued + 31 * DAY)
        self.assert_refused(image, self.policy(), 'expired')

    def test_a_certificate_not_yet_valid_is_refused(self):
        image = self.publish('early')
        self.attach(image, cert='os-images-1', integrated=self.issued - DAY)
        self.assert_refused(image, self.policy(), 'not yet valid')

    def test_another_root_is_refused(self):
        image = self.publish('foreign-root')
        self.attach(image, cert='os-images-1')
        self.assert_refused(image, self.policy(root='other-root'), 'unknown authority')

    def test_raising_the_epoch_revokes_earlier_certificates(self):
        revoked, current = self.publish('revoked'), self.publish('current')
        self.attach(revoked, cert='os-images-1')
        self.attach(current, cert='os-images-2')
        policy = self.policy(trust=self.trust_epoch_2)
        self.assert_refused(revoked, policy, 'OIDC issuer')
        self.assert_accepted(current, policy)

    def test_a_certificate_for_another_purpose_is_refused(self):
        image = self.publish('extension-certificate')
        self.attach(image, cert='extensions-1')
        self.assert_refused(image, self.policy(), 'os-images@')

    def test_an_entry_of_another_log_is_refused(self):
        image = self.publish('foreign-log')
        self.attach(image, cert='os-images-1', rekor='other-rekor')
        self.assert_refused(image, self.policy(), 'SET')

    def test_a_signature_outside_the_log_is_refused(self):
        image = self.publish('unlogged')
        self.attach(image, cert='os-images-1', logged=False)
        self.assert_refused(image, self.policy(), 'bundle')
        self.attach(image, logged=False)
        self.assert_refused(image, self.policy(chain=False), 'bundle')

    def test_key_mode_takes_the_bare_signature_only(self):
        bare, certified = self.publish('bare'), self.publish('certified')
        self.attach(bare)
        self.attach(certified, cert='os-images-1')
        policy = self.policy(chain=False)
        self.assert_accepted(bare, policy)
        self.assert_refused(certified, policy, 'does not match')

    def test_the_ceremony_refuses_what_it_must_not_issue(self):
        root, key = self.work / 'root.crt', self.keys['root']
        for args, reason in (
                (('--purpose', 'os-images', '--public-key', self.public('root'), '--days', '30'),
                 b'is the root key'),
                (('--purpose', 'os-images', '--public-key', self.public('operational'), '--days', '91'),
                 b'--days must be'),
                (('--purpose', 'anything', '--public-key', self.public('operational')),
                 b'unknown purpose')):
            with self.subTest(reason=reason):
                result = subprocess.run(['bash', str(CEREMONY), 'issue', '--root', str(root),
                                         '--key', str(key), '--out', str(self.work / 'refused.crt'),
                                         *map(str, args)], capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(reason, result.stderr)
        # verify reports the purpose and epoch a certificate was issued for.
        self.ceremony('verify', '--root', root, '--purpose', 'os-images', self.work / 'os-images-1.crt')
        with self.assertRaises(subprocess.CalledProcessError):
            self.ceremony('verify', '--root', root, '--purpose', 'os-images',
                          self.work / 'os-images-1.crt', trust=self.trust_epoch_2)
        with self.assertRaises(subprocess.CalledProcessError):
            self.ceremony('verify', '--root', self.work / 'other-root.crt', '--purpose', 'os-images',
                          self.work / 'os-images-1.crt')

    def test_revoke_raises_the_epoch(self):
        trust = self.work / 'trust-revoke.env'
        shutil.copyfile(release_trust.TRUST_ENV, trust)
        self.ceremony('revoke', trust=trust)
        self.assertIn(f'TILO_RELEASE_EPOCH={release_trust.epoch() + 1}\n', trust.read_text())

    def test_ci_checks_the_certificates_it_signs_with(self):
        operational = self.work / 'operational-dir'
        operational.mkdir(exist_ok=True)
        (operational / 'os-images.crt').write_bytes(self.certs['os-images-1'])
        with mock.patch.object(release_trust, 'ROOT', self.work / 'root.crt'), \
                mock.patch.object(release_trust, 'OPERATIONAL', operational):
            self.assertEqual(release_trust.signing_certs('os-images', self.public('operational')),
                             [(operational / 'os-images.crt', 1)])
            with self.assertRaisesRegex(ValueError, 'another key'):
                release_trust.signing_certs('os-images', self.public('rekor'))
            # A certificate with less validity left than signing requires.
            strict = self.work / 'trust-strict.env'
            strict.write_text(release_trust.TRUST_ENV.read_text().replace(
                'TILO_RELEASE_OPERATIONAL_MIN_REMAINING_DAYS=', 'TILO_RELEASE_OPERATIONAL_MIN_REMAINING_DAYS=31\n#'))
            with mock.patch.object(release_trust, 'TRUST_ENV', strict), \
                    self.assertRaisesRegex(ValueError, 'expires within'):
                release_trust.signing_certs('os-images', self.public('operational'))
            (operational / 'os-images.crt').write_bytes(self.certs['extensions-1'])
            with self.assertRaisesRegex(ValueError, 'does not name'):
                release_trust.signing_certs('os-images', self.public('operational'))


if __name__ == '__main__':
    unittest.main()
