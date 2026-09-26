#!/usr/bin/env python3
"""The shipped container policy must verify every Tilo OS repository it names.

The policy default is insecureAcceptAnything, so a repository that is missing
from it is not "unverified by accident" — bootc would install whatever the
registry serves. Every repository a Tilo system can be pointed at therefore
needs an explicit entry, and every signed entry needs the attachment lookup
and the release workflow to sign under exactly that name (the signature
identity is matchRepository).
"""
import importlib.util
import json
from pathlib import Path
import re
import unittest

REPO = Path(__file__).resolve().parents[2]
POLICY = REPO / 'os/etc/containers/policy.json'
REGISTRIES_D = REPO / 'os/etc/containers/registries.d'
RELEASE_WORKFLOW = REPO / '.forgejo/workflows/build-image.yml'
IMAGE_RECIPES = (REPO / 'os/Containerfile', REPO / 'os/Containerfile.host')
KEY_PATH = '/usr/share/tilo/cosign.pub'
UNSIGNED_REPOSITORIES = ('tilo-vm', 'tilo-ci', 'tilo-dev')

spec = importlib.util.spec_from_file_location('hardware_profile', REPO / 'os/dev/hardware_profile.py')
hardware_profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hardware_profile)


def docker_scopes():
    policy = json.loads(POLICY.read_text(encoding='utf-8'))
    return policy, policy['transports']['docker']


def signed_repositories():
    _, scopes = docker_scopes()
    return sorted(scope for scope, requirements in scopes.items()
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


class ContainerSignaturePolicyTests(unittest.TestCase):
    def test_default_stays_explicit(self):
        policy, _ = docker_scopes()
        # The default is permissive by design (arbitrary user containers);
        # every assertion below exists because of it.
        self.assertEqual(policy['default'], [{'type': 'insecureAcceptAnything'}])

    def test_signed_repositories_pin_the_release_key_and_repository_identity(self):
        _, scopes = docker_scopes()
        self.assertTrue(signed_repositories(), 'no Tilo repository requires a signature')
        for scope in signed_repositories():
            with self.subTest(scope=scope):
                self.assertEqual(scopes[scope], [{
                    'type': 'sigstoreSigned',
                    'keyPath': KEY_PATH,
                    'signedIdentity': {'type': 'matchRepository'},
                }])

    def test_every_owner_namespace_with_a_signed_os_rejects_unsigned_images(self):
        _, scopes = docker_scopes()
        for scope in signed_repositories():
            namespace = scope.rsplit('/', 1)[0]
            for name in UNSIGNED_REPOSITORIES:
                with self.subTest(namespace=namespace, repository=name):
                    self.assertEqual(scopes.get(f'{namespace}/{name}'), [{'type': 'reject'}],
                                     f'{namespace}/{name} would be accepted unsigned')

    def test_no_scope_weakens_a_tilo_repository(self):
        _, scopes = docker_scopes()
        for scope, requirements in scopes.items():
            with self.subTest(scope=scope):
                self.assertNotIn({'type': 'insecureAcceptAnything'}, requirements)

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

    def test_image_recipes_install_every_attachment_file(self):
        for recipe in IMAGE_RECIPES:
            text = recipe.read_text(encoding='utf-8')
            for path in sorted(REGISTRIES_D.glob('*.yaml')):
                with self.subTest(recipe=recipe.name, file=path.name):
                    self.assertIn(f'os/etc/containers/registries.d/{path.name}', text)
                    self.assertIn(f'/etc/containers/registries.d/{path.name}', text)

    def test_release_workflow_signs_under_a_policy_repository(self):
        # matchRepository compares the repository recorded in the signature
        # with the one being pulled, so an image signed under any other name
        # would be refused by every installed system.
        text = RELEASE_WORKFLOW.read_text(encoding='utf-8')
        targets = set(re.findall(r'cosign sign --yes --key "\$COSIGN_KEY_REF" \\\s*"([^"@]+)@', text))
        self.assertTrue(targets, 'release workflow no longer signs the OS image')
        for target in targets:
            with self.subTest(target=target):
                self.assertIn(target, signed_repositories())

    def test_default_update_origin_requires_a_signature(self):
        repository = re.split(r'[:@]', hardware_profile.DEFAULT_TARGET_IMGREF, maxsplit=1)[0]
        self.assertIn(repository, signed_repositories())


if __name__ == '__main__':
    unittest.main()
