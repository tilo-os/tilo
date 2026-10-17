#!/usr/bin/env python3
"""The shipped container policy is the one tools/signing/release_trust.py
generates, and it verifies every Tilo repository an installed system pulls.

The default is `reject`: an image reference nobody listed is refused, and each
listed one is verified, refused, or accepted for a stated reason (the module
docstring and lists of release_trust.py). Verified repositories need a
transparency-logged signature, from the release key before the root ceremony
and through an operational certificate to the release root after it; they also
need the attachment lookup, and the t products that publish them must sign with
a log entry, which is what makes oci-sign.sh add the certified signatures.
"""
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import tempfile
import unittest
from unittest import mock

REPO = Path(__file__).resolve().parents[2]
POLICY = REPO / 'os/etc/containers/policy.json'
REGISTRIES_D = REPO / 'os/etc/containers/registries.d'
IMAGE_RECIPES = (REPO / 'os/Containerfile', REPO / 'os/Containerfile.host')
# The log ID of the public Rekor instance: SHA-256 of its public key.
PUBLIC_REKOR_LOG_ID = 'c0d23d6ad406973f9559f3ba2d1ca01f84147d8ffc5b8445c224f98b9591801d'


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


hardware_profile = load('hardware_profile', REPO / 'os/dev/hardware_profile.py')
release_trust = load('release_trust', REPO / 'tools/signing/release_trust.py')
products = load('products', REPO / 'tools/ci/products.py')


def shipped():
    return json.loads(POLICY.read_text(encoding='utf-8'))


def docker_scopes(policy=None):
    return (policy or shipped())['transports']['docker']


def signed_repositories(policy=None):
    return sorted(scope for scope, requirements in docker_scopes(policy).items()
                  if any(row.get('type') == 'sigstoreSigned' for row in requirements))


def attachment_repositories():
    """Repositories that a registries.d file routes to sigstore attachments.

    The files are two-level YAML maps; parse that shape strictly instead of
    depending on a YAML library in the CI image.
    """
    found = {}
    for path in sorted(REGISTRIES_D.glob('*.yaml')):
        lines = [line for line in path.read_text(encoding='utf-8').splitlines()
                 if line.strip() and not line.lstrip().startswith('#')]
        if not lines or lines[0] != 'docker:':
            raise AssertionError(f'{path.name} is not a docker registries.d map')
        scope = None
        for line in lines[1:]:
            if match := re.fullmatch(r'  "?([^":\s]+)"?:', line):
                scope = match.group(1)
            elif match := re.fullmatch(r'    use-sigstore-attachments: (true|false)', line):
                if scope is None:
                    raise AssertionError(f'{path.name} sets attachments outside a scope')
                found[scope] = (match.group(1) == 'true', path.name)
            else:
                raise AssertionError(f'{path.name} has an unexpected line: {line!r}')
    return found


def pem_der(text):
    body = ''.join(line for line in text.splitlines() if not line.startswith('-----'))
    return base64.b64decode(body, validate=True)


class ContainerSignaturePolicyTests(unittest.TestCase):
    def test_shipped_policy_is_generated(self):
        self.assertEqual(POLICY.read_text(encoding='utf-8'), release_trust.render(),
                         'run python3 tools/signing/release_trust.py policy')

    def test_default_refuses_unlisted_references(self):
        self.assertEqual(shipped()['default'], [{'type': 'reject'}])

    def test_signed_repositories_require_a_logged_release_signature(self):
        scopes = docker_scopes()
        self.assertEqual(signed_repositories(), sorted(release_trust.SIGNED_REPOSITORIES))
        for scope in signed_repositories():
            with self.subTest(scope=scope):
                expected = {'type': 'sigstoreSigned',
                            'rekorPublicKeyPath': '/usr/share/tilo/release/rekor.pub',
                            'signedIdentity': {'type': 'matchRepository'}}
                if release_trust.mode() == 'chain':
                    expected['fulcio'] = {'caPath': '/usr/share/tilo/release/root.crt',
                                          'oidcIssuer': release_trust.issuer(),
                                          'subjectEmail': release_trust.identity('os-images')}
                else:
                    expected['keyPath'] = '/usr/share/tilo/cosign.pub'
                self.assertEqual(scopes[scope], [expected])

    def test_chain_mode_anchors_every_signed_repository_in_the_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'root.crt'
            root.write_text('placeholder\n')
            with mock.patch.object(release_trust, 'ROOT', root):
                policy = release_trust.policy()
        domain = release_trust.trust()['TILO_RELEASE_IDENTITY_DOMAIN']
        for scope in release_trust.SIGNED_REPOSITORIES:
            with self.subTest(scope=scope):
                [requirement] = policy['transports']['docker'][scope]
                self.assertNotIn('keyPath', requirement)
                self.assertEqual(requirement['fulcio'], {
                    'caPath': '/usr/share/tilo/release/root.crt',
                    'oidcIssuer': release_trust.trust()['TILO_RELEASE_ISSUER_PREFIX']
                    + release_trust.trust()['TILO_RELEASE_EPOCH'],
                    'subjectEmail': f'os-images@{domain}'})
                self.assertEqual(requirement['rekorPublicKeyPath'], '/usr/share/tilo/release/rekor.pub')

    def test_development_repositories_are_refused_by_name(self):
        scopes = docker_scopes()
        for scope in signed_repositories():
            namespace = scope.rsplit('/', 1)[0]
            for name in ('tilo-vm', 'tilo-ci', 'tilo-dev'):
                with self.subTest(namespace=namespace, repository=name):
                    self.assertEqual(scopes.get(f'{namespace}/{name}'), [{'type': 'reject'}])

    def test_unverified_references_are_only_the_listed_ones(self):
        accept = [{'type': 'insecureAcceptAnything'}]
        policy = shipped()
        accepted = {(transport, scope) for transport, scopes in policy['transports'].items()
                    for scope, requirements in scopes.items() if requirements == accept}
        expected = {('docker', registry) for registry in release_trust.USER_CHOSEN_REGISTRIES}
        expected |= {(transport, '') for transport in release_trust.LOCAL_TRANSPORTS}
        self.assertEqual(accepted, expected)
        for transport, scopes in policy['transports'].items():
            for scope, requirements in scopes.items():
                with self.subTest(transport=transport, scope=scope):
                    self.assertTrue(requirements == accept or all(
                        row['type'] in ('reject', 'sigstoreSigned') for row in requirements))
        for registry in release_trust.USER_CHOSEN_REGISTRIES:
            with self.subTest(registry=registry):
                self.assertNotIn('/', registry)
                self.assertFalse(registry.endswith('tilo.so'))

    def test_signed_repositories_look_up_sigstore_attachments(self):
        # registries.d scopes are a host, a namespace or a repository; the
        # most specific scope containing the repository decides.
        attachments = attachment_repositories()
        for repository in signed_repositories():
            with self.subTest(repository=repository):
                scopes = [scope for scope in attachments
                          if repository == scope or repository.startswith(scope + '/')]
                self.assertTrue(scopes, f'{repository} has no use-sigstore-attachments entry')
                enabled, _ = attachments[max(scopes, key=len)]
                self.assertTrue(enabled, f'{repository} disables sigstore attachments')

    def test_image_recipes_install_the_policy_inputs(self):
        for recipe in IMAGE_RECIPES:
            text = recipe.read_text(encoding='utf-8')
            for path in sorted(REGISTRIES_D.glob('*.yaml')):
                with self.subTest(recipe=recipe.name, file=path.name):
                    self.assertIn(f'os/etc/containers/registries.d/{path.name}', text)
                    self.assertIn(f'/etc/containers/registries.d/{path.name}', text)
            with self.subTest(recipe=recipe.name, file='keys/release'):
                self.assertIn('keys/release', text)
                self.assertIn('/usr/share/tilo/release', text)

    def test_policy_paths_are_what_the_image_installs(self):
        installed = {'/usr/share/tilo/cosign.pub': REPO / 'keys/cosign.pub',
                     '/usr/share/tilo/release/rekor.pub': REPO / 'keys/release/rekor.pub',
                     '/usr/share/tilo/release/root.crt': REPO / 'keys/release/root.crt'}
        for scope in signed_repositories():
            [requirement] = docker_scopes()[scope]
            for path in (requirement.get('keyPath'), requirement['rekorPublicKeyPath'],
                         requirement.get('fulcio', {}).get('caPath')):
                if path:
                    with self.subTest(scope=scope, path=path):
                        self.assertTrue(installed[path].is_file())

    def test_rekor_key_is_the_public_instance(self):
        der = pem_der((REPO / 'keys/release/rekor.pub').read_text(encoding='ascii'))
        self.assertEqual(hashlib.sha256(der).hexdigest(), PUBLIC_REKOR_LOG_ID)

    def test_trust_file_is_well_formed(self):
        trust = release_trust.trust()
        self.assertGreaterEqual(release_trust.epoch(), 1)
        self.assertTrue(trust['TILO_RELEASE_ISSUER_PREFIX'].startswith('https://'))
        self.assertRegex(trust['TILO_RELEASE_IDENTITY_DOMAIN'], r'^[a-z0-9.-]+$')
        self.assertLess(int(trust['TILO_RELEASE_OPERATIONAL_MIN_REMAINING_DAYS']),
                        int(trust['TILO_RELEASE_OPERATIONAL_MAX_DAYS']))
        ceremony = (REPO / 'tools/signing/release-ca.sh').read_text(encoding='utf-8')
        self.assertIn('PURPOSES=(' + ' '.join(release_trust.PURPOSES) + ')', ceremony)

    def test_every_signer_knows_the_verified_repositories(self):
        lib = (REPO / 'tools/ci/publish/lib.sh').read_text(encoding='utf-8')
        listed = re.search(r'device_verified\(\) \{\n  case \$1 in\n    ([^)]*)\)', lib).group(1)
        self.assertEqual({name.strip() for name in listed.split('|')},
                         set(release_trust.SIGNED_REPOSITORIES))
        self.assertEqual(products.DEVICE_VERIFIED, set(release_trust.SIGNED_REPOSITORIES))

    def test_verified_repositories_publish_logged_signatures(self):
        # A product publishing to a repository installed systems verify must
        # sign with a transparency-log entry: that is when oci-sign.sh adds the
        # certified signatures the chain-mode policy requires.
        published = [product for product in products.load().get('product', [])
                     if product.get('repository') in release_trust.SIGNED_REPOSITORIES]
        self.assertTrue(published, 'no t product publishes a verified repository')
        for product in published:
            with self.subTest(product=product['name']):
                self.assertTrue(product.get('release'), 'a verified repository is published signed')
                self.assertTrue(product.get('tlog'), 'its signature must be logged')

    def test_default_update_origin_requires_a_signature(self):
        repository = re.split(r'[:@]', hardware_profile.DEFAULT_TARGET_IMGREF, maxsplit=1)[0]
        self.assertIn(repository, signed_repositories())


if __name__ == '__main__':
    unittest.main()
