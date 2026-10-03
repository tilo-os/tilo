import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
spec = importlib.util.spec_from_file_location('stage', Path(sys.path[0]) / 'stage-kernel-checkpoint.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class Stage(unittest.TestCase):
    def fixture(self, source):
        ref = 'registry.tilo.so/tilo/tilo-development-partial-kernel-aarch64@sha256:' + 'a' * 64
        approval = dict(purpose='tilo-kbuild-reconstruction-admission', source_revision=m.restore.SOURCE,
                        checkpoint_reference=ref, restore_helper_sha256=m.restore.sha(Path(m.restore.__file__)))
        for flag in ('writer_mapping_complete', 'dependency_invalidation_reviewed',
                     'toolchain_identity_required', 'excluded_selftests_not_build_inputs'):
            approval[flag] = True
        for name, key in [('tree.tar', 'archive_sha256'), ('scan.json', 'scan_sha256'),
                          ('capture-receipt.json', 'capture_receipt_sha256'), ('toolchain.json', 'toolchain_evidence_sha256')]:
            (source / name).write_bytes(name.encode())
            approval[key] = m.restore.sha(source / name)
        return ref, approval

    def test_checkpoint_staging(self):
        with self.subTest(phase="exact_admitted_files_and_public_approval_staged"):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp); source = root / 'public'; source.mkdir()
                ref, approval = self.fixture(source)
                raw = json.dumps(approval).encode()
                m.stage(source, root / 'staged', raw, hashlib.sha256(raw).hexdigest(), ref)
                self.assertEqual((root / 'staged/approval.json').read_bytes(), raw)
                self.assertEqual((root / 'staged/tree.tar').stat().st_ino, (source / 'tree.tar').stat().st_ino)
        with self.subTest(phase="reject_changed_archive_unreviewed_flag_and_different_reference"):
            for mutation in ('archive', 'flag', 'reference', 'hash'):
                with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp); source = root / 'public'; source.mkdir()
                    ref, approval = self.fixture(source)
                    if mutation == 'archive': (source / 'tree.tar').write_bytes(b'changed')
                    if mutation == 'flag': approval['writer_mapping_complete'] = False
                    if mutation == 'reference': approval['checkpoint_reference'] = ref[:-1] + 'b'
                    raw = json.dumps(approval).encode()
                    digest = '0' * 64 if mutation == 'hash' else hashlib.sha256(raw).hexdigest()
                    with self.assertRaises(ValueError): m.stage(source, root / 'staged', raw, digest, ref)
                    self.assertFalse((root / 'staged').exists())


if __name__ == '__main__': unittest.main()
