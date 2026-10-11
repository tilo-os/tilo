import importlib.util
import os
from pathlib import Path
import re
import tomllib
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
PINS_FILE = ROOT / 'tools/ci/images.toml'
def load_pins():
    with PINS_FILE.open('rb') as stream:
        return tomllib.load(stream)


def load_resolver():
    spec = importlib.util.spec_from_file_location('pinned_resolver', ROOT / 'tools/ci/resolve_builder.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CiImagePins(unittest.TestCase):
    def test_filesystem_public_builder_jobs_do_not_require_forge_credentials(self):
        source = (ROOT / '.forgejo/workflows/ci-filesystem.yml').read_text()
        observed = set()
        jobs = re.findall(r'(?ms)^  ([a-z0-9_-]+):\n(.*?)(?=^  [a-z0-9_-]+:|\Z)', source)
        for name, job in jobs:
            container = re.search(r'(?ms)^    container:\n(.*?)(?=^    \S|\Z)', job)
            if container and 'image: ${{ needs.builder.outputs.image }}' in container[1]:
                observed.add(name)
                self.assertNotIn('credentials:', container[1], name)
        self.assertEqual(observed, {'real-fuse', 'cloud-servers', 'cloud-live', 'native-tier1', 'files-ux'})
        self.assertIn('TILO_REQUIRE_CLOUD_SERVERS: "1"', source)
        self.assertIn('TILO_LIVE_NEXTCLOUD_PASSWORD: ${{ secrets.TILO_LIVE_NEXTCLOUD_PASSWORD }}', source)

    def test_every_workflow_reference_uses_the_shared_pin(self):
        config = load_pins()
        registry = config['registry']['host'] + '/' + config['registry']['namespace'] + '/'
        pins = config['pins']
        for repository, digest in pins.items():
            self.assertRegex(digest, r'^sha256:[0-9a-f]{64}$', repository)
        reference = re.compile(re.escape(registry) + r'([a-z0-9._-]+)@(sha256:[0-9a-f]{64})')
        sources = sorted((ROOT / '.forgejo').rglob('*.yml'))
        self.assertTrue(sources)
        seen = set()
        for path in sources:
            for repository, digest in reference.findall(path.read_text()):
                with self.subTest(workflow=path.name, image=repository):
                    self.assertIn(repository, pins, 'add the image to tools/ci/images.toml')
                    self.assertEqual(digest, pins[repository], 'pin it in tools/ci/images.toml')
                    seen.add(repository)
        self.assertEqual(set(pins), seen, 'tools/ci/images.toml pins an image no workflow uses')

    def test_builder_resolution_reads_the_registry_from_the_pins_file(self):
        registry = load_pins()['registry']
        with patch.dict(os.environ, {}, clear=False):
            for name in ('TILO_CI_REGISTRY', 'TILO_CI_BUILDER_REPOSITORY', 'TILO_CI_REGISTRY_TOKEN_REALM'):
                os.environ.pop(name, None)
            resolver = load_resolver()
        repository = registry['namespace'] + '/' + registry['builder']
        self.assertEqual(resolver.IMAGE, registry['host'] + '/' + repository)
        self.assertEqual(resolver.MANIFEST_URL,
                         'https://' + registry['host'] + '/v2/' + repository + '/manifests/latest')
        self.assertEqual(resolver.TOKEN_REALM, registry['token_realm'])
        self.assertEqual(resolver.PULL_SCOPE, 'repository:' + repository + ':pull')

    def test_builder_registry_is_overridable_per_run(self):
        environ = {'TILO_CI_REGISTRY': 'ghcr.io', 'TILO_CI_BUILDER_REPOSITORY': 'example/tilo-builder',
                   'TILO_CI_REGISTRY_TOKEN_REALM': 'https://ghcr.io/token'}
        with patch.dict(os.environ, environ):
            resolver = load_resolver()
        self.assertEqual(resolver.IMAGE, 'ghcr.io/example/tilo-builder')
        self.assertEqual(resolver.MANIFEST_URL, 'https://ghcr.io/v2/example/tilo-builder/manifests/latest')
        self.assertEqual(resolver.TOKEN_REALM, 'https://ghcr.io/token')
        self.assertEqual(resolver.resolve('ghcr.io/example/tilo-builder@sha256:' + 'a' * 64),
                         'ghcr.io/example/tilo-builder@sha256:' + 'a' * 64)
        for bad in ({'TILO_CI_REGISTRY': 'evil.example/path'},
                    {'TILO_CI_BUILDER_REPOSITORY': 'Upper/Case'},
                    {'TILO_CI_REGISTRY_TOKEN_REALM': 'http://ghcr.io/token'}):
            with self.subTest(override=bad), patch.dict(os.environ, bad), self.assertRaises(ValueError):
                load_resolver()


if __name__ == '__main__':
    unittest.main()
