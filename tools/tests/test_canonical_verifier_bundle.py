import copy
import io
import tarfile
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
sys.path.insert(0, str(Path(__file__).parents[1]))
sys.path.append(str(Path(__file__).parents[1] / 'apps'))
import canonical_verifier_bundle as bundle
import final_app_receipt as final
import test_app_channel_manifest as fixtures

BUILDER = 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'a' * 64


def executable(path, machine=62):
    raw = bytearray(20)
    raw[:6] = b'\x7fELF\x02\x01'
    raw[18:20] = machine.to_bytes(2, 'little')
    path.write_bytes(raw + b'verifier fixture')


class BundleTests(unittest.TestCase):
    def fixture(self, root):
        pkg, runtime = root / 'pkg', root / 'runtime'
        executable(pkg)
        executable(runtime)
        archive = root / 'verifier.tar.gz'
        descriptor = bundle.produce(pkg, runtime, archive, BUILDER, 'b' * 40, 'x86_64')
        return archive, descriptor

    def test_bundle_producer_and_reader_accept_only_authentic_native_bundles(self):
        with self.subTest(phase='producer deterministic and reusable without current app binaries'):
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                archive, descriptor = self.fixture(root)
                other = root / 'other.tar.gz'
                second = bundle.produce(root / 'pkg', root / 'runtime', other, BUILDER, 'b' * 40, 'x86_64')
                self.assertEqual(descriptor, second)
                (root / 'pkg').write_bytes(b'new application-build compiler output')
                (root / 'runtime').unlink()
                materialized = bundle.materialize(archive, descriptor, root / 'installed', BUILDER, 'x86_64')
                self.assertEqual(bundle.channel.sha(materialized / 'tilo-app-package'), descriptor['canonical_binaries']['tilo_pkg_sha256'])
        with self.subTest(phase='tampered archive binary policy and incompatible builder rejected'):
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                archive, descriptor = self.fixture(root)
                with self.assertRaisesRegex(ValueError, 'compatibility'):
                    bundle.materialize(archive, descriptor, root / 'wrong-builder', BUILDER.replace('a' * 64, 'c' * 64), 'x86_64')
                wrong = copy.deepcopy(descriptor)
                wrong['canonical_binaries']['tilo_pkg_sha256'] = '0' * 64
                with self.assertRaisesRegex(ValueError, 'identity mismatch'):
                    bundle.materialize(archive, wrong, root / 'wrong-policy', BUILDER, 'x86_64')
                with self.assertRaisesRegex(ValueError, 'identity mismatch'):
                    bundle.materialize(archive, descriptor, root / 'wrong-arch', BUILDER, 'aarch64')
                archive.write_bytes(archive.read_bytes() + b'tamper')
                with self.assertRaisesRegex(ValueError, 'digest/size'):
                    bundle.materialize(archive, descriptor, root / 'tampered', BUILDER, 'x86_64')
        with self.subTest(phase='authenticated bundle still rejects links and corrupt executable'):
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                archive, descriptor = self.fixture(root)
                with tarfile.open(archive, 'r:gz') as original:
                    contents = {member.name: original.extractfile(member).read() for member in original}
                for kind in ('link', 'binary'):
                    altered = root / f'{kind}.tar.gz'
                    with tarfile.open(altered, 'w:gz') as output:
                        for name, data in contents.items():
                            info = tarfile.TarInfo(name)
                            if name == 'tilo-app-package' and kind == 'link':
                                info.type = tarfile.SYMTYPE
                                info.linkname = '/outside'
                                output.addfile(info)
                            else:
                                if name == 'tilo-app-package':
                                    data += b'tamper'
                                info.size = len(data)
                                output.addfile(info, io.BytesIO(data))
                    changed = dict(descriptor, sha256=bundle.channel.sha(altered), bytes=altered.stat().st_size)
                    with self.subTest(kind=kind), self.assertRaisesRegex(ValueError, 'unsafe bundle|executable digest'):
                        bundle.materialize(altered, changed, root / kind, BUILDER, 'x86_64')
        with self.subTest(phase='source producer rejects foreign isa'):
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                executable(root / 'pkg', 183)
                executable(root / 'runtime', 183)
                with self.assertRaisesRegex(ValueError, 'native ELF'):
                    bundle.produce(root / 'pkg', root / 'runtime', root / 'bad.tar.gz', BUILDER, 'b' * 40, 'x86_64')
        with self.subTest(phase='reader requires approved bundle and final policy selects native tools'):
            reader = fixtures.ChannelReaderTests()
            reader.setUp()
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                _, descriptor = self.fixture(root)
                policy = dict(reader.policy, verifier_sha256=descriptor['verifier_sha256'], canonical_verifier_bundles={'x86_64': descriptor})
                policy_file = root / 'policy.json'
                policy_file.write_text(json.dumps(policy))
                with patch.object(bundle, 'native_arch', return_value='x86_64'):
                    effective = final.load_policy(policy_file, bundle.channel.sha(policy_file))
                self.assertEqual(effective['canonical_binaries'], descriptor['canonical_binaries'])
                receipt = reader.published()
                receipt['verification']['verifier_sha256'] = descriptor['verifier_sha256']
                receipt['verification'].update(backend='canonical-cli-v1', canonical_binaries=descriptor['canonical_binaries'],
                    canonical_bundle={'architecture': 'x86_64', 'sha256': descriptor['sha256']})
                self.assertTrue(reader.select(reader.manifest(receipt), policy=policy)['usable_for_download'])
                receipt['verification']['canonical_bundle']['sha256'] = '0' * 64
                with self.assertRaisesRegex(ValueError, 'bundle not approved'):
                    reader.select(reader.manifest(receipt), policy=policy)
                with patch.object(bundle, 'native_arch', return_value='aarch64'), self.assertRaises(KeyError):
                    final.load_policy(policy_file, bundle.channel.sha(policy_file))

    def test_failed_fetch_does_not_poison_retry_or_touch_sibling_archive(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            archive, descriptor = self.fixture(root)
            raw = archive.read_bytes()
            policy = root / 'policy.json'
            policy.write_text(json.dumps({'verifier_sha256': descriptor['verifier_sha256'],
                                         'canonical_verifier_bundles': {'x86_64': descriptor}}))
            for failure in ('truncated', 'oversized', 'read-error', 'interrupted'):
                with self.subTest(failure=failure):
                    output = root / failure
                    sibling = output.with_suffix('.tar.gz')
                    # A pre-existing sibling belongs to the caller, not fetch.
                    if failure == 'interrupted':
                        sibling.write_bytes(b'preserve')
                    response = io.BytesIO(raw[:10] if failure == 'truncated' else raw + b'extra')
                    if failure in ('read-error', 'interrupted'):
                        error = OSError('connection lost') if failure == 'read-error' else KeyboardInterrupt()
                        response.read = Mock(side_effect=[raw[:10], error])
                    with patch.object(bundle.urllib.request, 'build_opener') as network:
                        network.return_value.open.return_value = response
                        with self.assertRaises((ValueError, OSError, KeyboardInterrupt)):
                            bundle.fetch(policy, bundle.channel.sha(policy), output, BUILDER, 'secret', 'x86_64')
                        self.assertFalse(output.exists())
                        if failure == 'interrupted':
                            self.assertEqual(sibling.read_bytes(), b'preserve')
                        else:
                            self.assertFalse(sibling.exists())
                        network.return_value.open.return_value = io.BytesIO(raw)
                        bundle.fetch(policy, bundle.channel.sha(policy), output, BUILDER, 'secret', 'x86_64')
                    self.assertEqual(bundle.channel.sha(output / 'tilo-app-package'),
                                     descriptor['canonical_binaries']['tilo_pkg_sha256'])
                    if sibling.exists():
                        self.assertEqual(sibling.read_bytes(), b'preserve')

    def test_bundle_is_fetched_pinned_imported_frozen_and_consumed_per_platform(self):
        with self.subTest(phase='preflight imports frozen bundle adapter not candidate copy'):
            reader = fixtures.ChannelReaderTests()
            reader.setUp()
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                archive, descriptor = self.fixture(root)
                installed = bundle.materialize(archive, descriptor, root / 'platform', BUILDER, 'x86_64')
                candidate = root / 'candidate'
                candidate.mkdir()
                (candidate / 'canonical_app_verifier.py').write_text('raise AssertionError("candidate executed")')
                policy = dict(reader.policy, verifier_sha256=descriptor['verifier_sha256'],
                              canonical_verifier_bundles={'x86_64': descriptor})
                policy_path = root / 'policy.json'
                policy_path.write_text(json.dumps(policy))
                with patch.object(bundle, 'native_arch', return_value='x86_64'), patch.object(final, '__file__', str(candidate / 'final_app_receipt.py')):
                    _, adapter = final.preflight(installed / 'tilo-app-package', installed / 'tilo-runtime-bundle',
                                                 policy_path, bundle.channel.sha(policy_path))
                self.assertEqual(adapter, installed / 'canonical_app_verifier.py')
        with self.subTest(phase='fetch refuses unpinned policy or mutable url before network'):
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                _, descriptor = self.fixture(root)
                descriptor['url'] = 'https://example.invalid/latest.tar.gz'
                policy = root / 'policy.json'
                policy.write_text(json.dumps({'verifier_sha256': descriptor['verifier_sha256'], 'canonical_verifier_bundles': {'x86_64': descriptor}}))
                with patch.object(bundle.urllib.request, 'build_opener') as network:
                    with self.assertRaisesRegex(ValueError, 'trusted digest'):
                        bundle.fetch(policy, '0' * 64, root / 'out', BUILDER, 'secret', 'x86_64')
                    with self.assertRaisesRegex(ValueError, 'URL must pin'):
                        bundle.fetch(policy, bundle.channel.sha(policy), root / 'out', BUILDER, 'secret', 'x86_64')
                    network.assert_not_called()
        with self.subTest(phase='workflow consumes platform bundle without shipping per build tools'):
            root = Path(__file__).parents[2]
            text = ((root / '.forgejo/workflows/release-apps.yml').read_text()
                    + (root / 'tools/apps/publish-collections.sh').read_text())
            self.assertNotIn('name: canonical-verifiers-', text)
            self.assertNotIn('cp target/release/tilo-app-package target/release/tilo-runtime-bundle', text)
            self.assertEqual(text.count('python3 -I tools/ci/python.py tools/canonical_verifier_bundle.py fetch'), 2)
            self.assertIn('    branches: [main]', text)


if __name__ == '__main__':
    unittest.main()
