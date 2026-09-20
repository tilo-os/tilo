"""Publication must follow independently checked handoff and layer audit."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
spec = importlib.util.spec_from_file_location('compile_retention_driver', DEV / 'compile-development-os.py')
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class RetentionBoundary(unittest.TestCase):
    def test_publication_requires_matching_epoch_and_successful_audit(self):
        image_id = 'sha256:' + 'c' * 64
        revision, manifest = 'a' * 40, 'b' * 64
        target = 'ghcr.io/tilo-os/tilo-development:testing-x86_64'
        handoff = {'schema': 1, 'purpose': 'tilo-development-raw-rootfs-handoff',
                   'raw_rootfs_id': image_id, 'revision': revision, 'source_date_epoch': 123,
                   'architecture': 'x86_64', 'generation': 7, 'target_update_ref': target,
                   'public_manifest_sha256': manifest, 'installable': False, 'sealed': False,
                   'published': False,
                   'validation': 'captured build inputs only; independently revalidate before reuse'}
        for status, epoch, architecture in [('passed-scoped-scan', 123, 'x86_64'),
                              ('passed-scoped-scan', 123, 'aarch64'),
                              ('rejected-private-material', 123, 'aarch64'),
                              ('passed-scoped-scan', 124, 'aarch64')]:
            handoff['architecture'] = architecture
            target = 'ghcr.io/tilo-os/tilo-development:testing-' + architecture
            handoff['target_update_ref'] = target
            with self.subTest(status=status, epoch=epoch), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                (directory / 'raw-rootfs.json').write_text(json.dumps(handoff))
                audit = {'image_id': image_id, 'status': status}
                published = {'handoff': handoff, 'published': True, 'installable': False}
                process = SimpleNamespace(returncode=0 if status == 'passed-scoped-scan' else 1,
                    stdout=json.dumps(audit).encode(), stderr=b'private diagnostic sentinel')
                controller = directory / 'current-controller'
                self.assertNotEqual(controller, driver.REPO)
                with patch.object(driver, 'EVIDENCE', directory), \
                     patch.object(driver, 'CONTROLLER', controller), \
                     patch.object(driver.subprocess, 'run', return_value=process) as scanner, \
                     patch.object(driver, 'run', return_value=json.dumps(published)) as publisher:
                    if status == 'passed-scoped-scan' and epoch == 123:
                        self.assertEqual(driver.retain_compilation(revision, '7', epoch, target, manifest, architecture), published)
                        publisher.assert_called_once()
                        command = publisher.call_args.args
                        self.assertEqual(command[command.index('--architecture') + 1], architecture)
                        self.assertTrue((directory / 'retained-rootfs.json').is_file())
                    else:
                        with self.assertRaises(ValueError):
                            driver.retain_compilation(revision, '7', epoch, target, manifest, architecture)
                        publisher.assert_not_called()
                        self.assertFalse((directory / 'retained-rootfs.json').exists())
                    if epoch != 123:
                        scanner.assert_not_called()
                    else:
                        # The frozen application checkout remains the working
                        # directory, but the reviewed controller owns the audit.
                        scan_command = scanner.call_args.args[0]
                        self.assertEqual(scan_command[:4], ['python3',
                            str(controller / 'os/dev/audit-rootfs-layers.py'), '--image-id', image_id])
                        self.assertTrue(Path(scan_command[1]).is_absolute())
                        self.assertEqual(scanner.call_args.kwargs['cwd'], driver.REPO)
                        self.assertEqual(json.loads((directory / 'raw-rootfs-audit.json').read_text()), audit)
                    for path in directory.iterdir():
                        self.assertNotIn('private diagnostic sentinel', path.read_text())

    def test_categorical_audit_failure_is_preserved_without_publication(self):
        image_id = 'sha256:' + 'c' * 64
        revision, manifest = 'a' * 40, 'b' * 64
        for architecture in ('x86_64', 'aarch64'):
            target = 'ghcr.io/tilo-os/tilo-development:testing-' + architecture
            handoff = {'schema': 1, 'purpose': 'tilo-development-raw-rootfs-handoff',
                       'raw_rootfs_id': image_id, 'revision': revision, 'source_date_epoch': 123,
                       'architecture': architecture, 'generation': 7, 'target_update_ref': target,
                       'public_manifest_sha256': manifest, 'installable': False, 'sealed': False,
                       'published': False,
                       'validation': 'captured build inputs only; independently revalidate before reuse'}
            audit = {'schema': 1, 'image_id': image_id, 'scanner': 'docker-layer-private-material-v1',
                     'status': 'failed-scoped-scan', 'failed_stage': 'layer-export-and-scan',
                     'failure_reason': 'archive-identity-mismatch',
                     'universal_secret_absence_proven': False, 'installable': False}
            process = SimpleNamespace(returncode=1, stdout=json.dumps(audit).encode(),
                                      stderr=b'private diagnostic sentinel')
            with self.subTest(architecture=architecture), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                (directory / 'raw-rootfs.json').write_text(json.dumps(handoff))
                with patch.object(driver, 'EVIDENCE', directory), \
                     patch.object(driver.subprocess, 'run', return_value=process), \
                     patch.object(driver, 'run') as publisher:
                    with self.assertRaisesRegex(ValueError, 'raw rootfs failed the private-material layer audit'):
                        driver.retain_compilation(revision, '7', 123, target, manifest, architecture)
                publisher.assert_not_called()
                self.assertEqual(json.loads((directory / 'raw-rootfs-audit.json').read_text()), audit)
                self.assertFalse((directory / 'retained-rootfs.json').exists())
                for path in directory.iterdir():
                    self.assertNotIn('private diagnostic sentinel', path.read_text())


if __name__ == '__main__':
    unittest.main()
