"""Execute the actual release invariant, including disconnected-builder negatives."""
from pathlib import Path
import hashlib
import re
import subprocess
import tempfile
import textwrap
import unittest
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parents[2]
PUBLISH_SCRIPT = ROOT / 'tools/apps/publish-collections.sh'


def bash_path(path: Path) -> str:
    """Return a path WSL bash and native Unix bash can both open."""
    rendered = path.resolve().as_posix()
    if re.match(r'^[A-Za-z]:/', rendered):
        return '/mnt/' + rendered[0].lower() + rendered[2:]
    return rendered


class AppReleaseWorkflowContract(unittest.TestCase):
    def test_collection_address_identifies_assembled_bytes_for_the_same_source(self):
        source = PUBLISH_SCRIPT.read_text()
        start = source.index('  bundle_sha=$(sha256sum "$bundle"')
        end = source.index('  if [[ ${VERIFY_FINAL_RECEIPT', start)
        address = source[start:end]
        with tempfile.TemporaryDirectory() as directory:
            bundle = Path(directory) / 'tilo-apps-x86_64.tar.gz'

            def urls(content):
                bundle.write_bytes(content)
                script = address + '\nprintf "%s\\n%s\\n" "$bundle_upload_url" "$bundle_url"\n'
                return subprocess.check_output(
                    ['bash', '-eu', '-c', script], text=True,
                    env={'PATH': '/usr/bin:/bin', 'bundle': bash_path(bundle),
                         'VERSION': '0.0.0-' + 'a' * 40,
                         'package_api': 'https://code.tilo.so/api/packages/tilo/generic',
                         'package_public_api': 'https://registry.tilo.so/artifacts/tilo/generic'},
                ).splitlines()

            first = urls(b'first selected app cohort')
            second = urls(b'second selected app cohort')
            self.assertNotEqual(first, second)
            self.assertEqual(first, urls(b'first selected app cohort'))
            for url in first:
                self.assertIn(hashlib.sha256(b'first selected app cohort').hexdigest(), url)
            self.assertNotIn('tilo-app-collections/$VERSION/', source)
            self.assertEqual(source.count('tilo-app-collections/$collection_version/'), 5)


    def coordinator_bootstrap(self):
        source = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
        script = source.split('# BEGIN coordinator preguard bootstrap\n', 1)[1].split(
            '          # END coordinator preguard bootstrap', 1)[0]
        namespace = {'__name__': 'contract_test'}
        exec(compile(textwrap.dedent(script), '<coordinator-bootstrap>', 'exec'), namespace)
        return namespace

    class Response:
        status = 200

        def __init__(self, url, payload):
            self.url = url
            self.payload = payload

        def __enter__(self):
            return self

        def __exit__(self, *args):
            return False

        def geturl(self):
            return self.url

        def read(self, size):
            return self.payload[:size]

    class Opener:
        def __init__(self, payload, response_url=None):
            self.payload = payload
            self.response_url = response_url
            self.request = None

        def open(self, request, timeout):
            self.request = request
            return AppReleaseWorkflowContract.Response(
                self.response_url or request.full_url, self.payload)

    def test_coordinator_bootstrap_fetches_only_the_exact_bounded_closure(self):
        with self.subTest(phase='coordinator bootstrap manifest matches exact runtime closure'):
            bootstrap = self.coordinator_bootstrap()
            self.assertEqual(
                [entry[0] for entry in bootstrap['FILES']],
                [
                    'tools/app_packaging_qualification.py',
                    'tools/app_packaging_contract.py',
                    'tools/ci/__init__.py',
                    'tools/ci/python.py',
                    'tools/ci/resolve_builder.py',
                    'tools/ci/images.toml',
                ],
            )
            for relative, size, digest in bootstrap['FILES']:
                payload = (ROOT / relative).read_bytes()
                self.assertEqual(len(payload), size, relative)
                self.assertEqual(hashlib.sha256(payload).hexdigest(), digest, relative)
        with self.subTest(phase='coordinator bootstrap rejects redirects'):
            bootstrap = self.coordinator_bootstrap()
            with self.assertRaisesRegex(ValueError, 'redirects are forbidden'):
                bootstrap['NoRedirect']().redirect_request(None, None, 302, '', {}, 'https://elsewhere.invalid')
        with self.subTest(phase='coordinator bootstrap rejects oversize source'):
            bootstrap = self.coordinator_bootstrap()
            with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, 'size or hash'):
                bootstrap['fetch_one'](
                    self.Opener(b'ab'), Path(directory), 'a' * 40, 'token',
                    'tools/example.py', 1, hashlib.sha256(b'a').hexdigest())
        with self.subTest(phase='coordinator bootstrap rejects hash mismatch'):
            bootstrap = self.coordinator_bootstrap()
            with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, 'size or hash'):
                bootstrap['fetch_one'](
                    self.Opener(b'a'), Path(directory), 'a' * 40, 'token',
                    'tools/example.py', 1, hashlib.sha256(b'b').hexdigest())
        with self.subTest(phase='coordinator bootstrap rejects path escape'):
            bootstrap = self.coordinator_bootstrap()
            with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, 'invalid.*path'):
                bootstrap['fetch_one'](
                    self.Opener(b'a'), Path(directory), 'a' * 40, 'token',
                    'tools/../escape.py', 1, hashlib.sha256(b'a').hexdigest())
        with self.subTest(phase='coordinator bootstrap requests only the exact ref'):
            bootstrap = self.coordinator_bootstrap()
            payload = b'exact source'
            opener = self.Opener(payload)
            with tempfile.TemporaryDirectory() as directory:
                bootstrap['fetch_one'](
                    opener, Path(directory), 'a' * 40, 'scoped-token',
                    'tools/example.py', len(payload), hashlib.sha256(payload).hexdigest())
                parsed = urlparse(opener.request.full_url)
                self.assertEqual(parse_qs(parsed.query), {'ref': ['a' * 40]})
                self.assertEqual(opener.request.get_header('Authorization'), 'token scoped-token')
                self.assertEqual((Path(directory) / 'tools/example.py').read_bytes(), payload)


if __name__ == '__main__':
    unittest.main()
