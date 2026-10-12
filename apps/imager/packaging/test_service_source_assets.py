"""Public subject admission and held-file measurements, not native session evidence."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).parent
spec = importlib.util.spec_from_file_location('public_assets', HERE / 'service-source-assets.py')
assets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assets)
spec = importlib.util.spec_from_file_location('asset_qualifier', HERE / 'qualify-capture-release.py')
qualifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualifier)


def ledger():
    return json.loads((HERE / 'service-source-subjects.json').read_text())


def located(root):
    result = ledger()
    for group, row in result['assets'].items():
        row['source_root'] = str(root / group)
    for key, row in result['archives'].items():
        row['source_path'] = str(root / key)
    return result


class SubjectIdentity(unittest.TestCase):
    def test_checked_in_subjects_and_copied_inputs_have_same_identity(self):
        expected = assets.ASSET_MANIFEST_SHA256
        self.assertEqual(assets.subject_digest(ledger()), expected)
        for root in (Path('/tmp/builder-one'), Path('/var/lib/builder-two')):
            document = located(root)
            self.assertEqual(assets.subject_digest(document), expected)
            with tempfile.TemporaryDirectory() as temporary:
                manifest = Path(temporary) / 'operator.json'
                manifest.write_text(json.dumps(document))
                self.assertEqual(qualifier.source_assets(manifest), document)
        legacy = located(Path('/tmp/legacy'))
        del legacy['distribution']
        self.assertEqual(assets.subject_digest(legacy), expected)

    def test_every_subject_fact_not_only_counts_is_pinned(self):
        original = located(Path('/tmp/public'))
        mutations = []
        for field, value in [('mode', 0o755), ('bytes', 1), ('sha256', '0' * 64)]:
            changed = copy.deepcopy(original)
            changed['assets']['app']['files']['package.json'][field] = value
            mutations.append(changed)
        for action in ('missing', 'extra', 'rename'):
            changed = copy.deepcopy(original)
            files = changed['assets']['app']['files']
            if action == 'missing':
                del files['package.json']
            elif action == 'extra':
                files['extra'] = dict(files['package.json'])
            else:
                files['renamed'] = files.pop('package.json')
            mutations.append(changed)
        for field, value in [('architecture', 'aarch64'), ('electron', '99'), ('slack', '99')]:
            changed = copy.deepcopy(original)
            changed['distribution'][field] = value
            mutations.append(changed)
        for changed in mutations:
            with self.subTest(document=changed['distribution']):
                with self.assertRaisesRegex(ValueError, 'official public asset manifest'):
                    assets.validate(changed)
                with self.assertRaises(ValueError):
                    assets.stage(changed, '/nonexistent')

    def test_upstream_archive_digest_is_independent_of_subject_pin(self):
        changed = located(Path('/tmp/public'))
        changed['archives']['electron_archive']['sha256'] = '0' * 64
        with patch.object(assets, 'ASSET_MANIFEST_SHA256', '0' * 64):
            with self.assertRaisesRegex(ValueError, 'upstream archive digest'):
                assets.validate(changed)
        for key in ('bytes', 'mode'):
            changed = located(Path('/tmp/public'))
            changed['archives']['slack_archive'][key] = 1
            with self.assertRaises(ValueError):
                assets.validate(changed)

    def test_locator_exclusion_is_not_an_unvalidated_extension_point(self):
        for value in ('relative', '/tmp/../etc', '/tmp//public'):
            changed = located(Path('/tmp/public'))
            changed['assets']['app']['source_root'] = value
            with self.assertRaises(ValueError):
                assets.validate(changed)
        changed = located(Path('/tmp/public'))
        changed['assets']['app']['ignored'] = 'untrusted'
        with self.assertRaises(ValueError):
            assets.validate(changed)
        changed = located(Path('/tmp/public'))
        changed['native_executed'] = True
        with self.assertRaisesRegex(ValueError, 'cannot claim native'):
            assets.validate(changed)


class MeasuredPublicFiles(unittest.TestCase):
    def test_complete_measurement_copy_modes_and_actual_corruption(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            document = located(root)
            # Small test subjects exercise the actual held-file production path;
            # patching admission does not claim these bytes are official distributions.
            archive_pins = {}
            for key, row in document['archives'].items():
                data = key.encode()
                path = Path(row['source_path'])
                path.write_bytes(data)
                path.chmod(0o644)
                row.update(bytes=len(data), mode=0o644, sha256=hashlib.sha256(data).hexdigest())
                archive_pins[key] = row['sha256']
            for group, row in document['assets'].items():
                path = Path(row['source_root'])
                path.mkdir(mode=0o755)
                data = group.encode()
                leaf = path / 'binary'
                leaf.write_bytes(data)
                leaf.chmod(0o755)
                row['files'] = {'binary': {'bytes': len(data), 'mode': 0o755,
                                          'sha256': hashlib.sha256(data).hexdigest()}}
            with patch.dict(assets.ARCHIVE_SHA256, archive_pins):
                pin = assets.subject_digest(document)
                with patch.object(assets, 'ASSET_MANIFEST_SHA256', pin):
                    self.assertEqual(assets.verify(document), document)
                    candidate = root / 'candidate'
                    candidate.mkdir(mode=0o700)
                    if os.geteuid() == 0:
                        receipt = assets.stage(document, candidate)
                        self.assertFalse(receipt['native_executed'])
                        self.assertEqual(receipt['files'], 3)
                    else:
                        with self.assertRaisesRegex(ValueError, 'actual root owner'):
                            assets.stage(document, candidate)
                    original = root / 'electron' / 'binary'
                    for change in ('extra', 'missing', 'mode', 'bytes', 'alias', 'archive'):
                        with self.subTest(change=change):
                            saved = original.read_bytes()
                            if change == 'extra':
                                (original.parent / 'extra').write_bytes(b'x')
                            elif change == 'missing':
                                original.unlink()
                            elif change == 'mode':
                                original.chmod(0o644)
                            elif change == 'bytes':
                                original.write_bytes(b'X' * len(saved))
                            elif change == 'alias':
                                original.unlink()
                                original.symlink_to(root / 'electron_archive')
                            else:
                                (root / 'electron_archive').write_bytes(b'X' * len(b'electron_archive'))
                            with self.assertRaises((ValueError, OSError)):
                                assets.verify(document)
                            if original.is_symlink():
                                original.unlink()
                            original.write_bytes(saved)
                            original.chmod(0o755)
                            (original.parent / 'extra').unlink(missing_ok=True)
                    self.assertNotEqual((root / 'electron_archive').read_bytes(), b'electron_archive')

    def test_destination_copy_is_measured_and_executable(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'source').mkdir()
            (root / 'destination').mkdir()
            leaf = root / 'source' / 'binary'
            leaf.write_bytes(b'public')
            leaf.chmod(0o755)
            expected = {'bytes': 6, 'mode': 0o755, 'sha256': hashlib.sha256(b'public').hexdigest()}
            source = assets.directory(root / 'source')
            target = assets.directory(root / 'destination')
            try:
                assets.measured_file(source, 'binary', expected, (target, 'binary'))
                assets.measured_file(target, 'binary', expected)
                self.assertEqual(assets.enumerate_files(target), {'binary'})
                with self.assertRaises(FileExistsError):
                    assets.measured_file(source, 'binary', expected, (target, 'binary'))
            finally:
                os.close(source)
                os.close(target)


if __name__ == '__main__':
    unittest.main()
