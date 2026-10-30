#!/usr/bin/env python3
"""Exercise actual Kbuild object selection, without compiling the kernel."""
from pathlib import Path
import subprocess
import hashlib
import importlib.util
import json
import sys
import tempfile

kernel = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(kernel.parent / 'dev'))
patch = kernel / "patches/0001-octeontx2-gate-cn20k-debugfs.patch"
spec = importlib.util.spec_from_file_location('restore', kernel.parent / 'dev/kernel_checkpoint_restore.py')
restore = importlib.util.module_from_spec(spec)
spec.loader.exec_module(restore)
approval = {'source_corrections': [{'name': patch.name, 'sha256': hashlib.sha256(patch.read_bytes()).hexdigest()}]}
original = (kernel / "tests/fixtures/octeontx2-af.Makefile").read_bytes()
with tempfile.TemporaryDirectory(prefix="tilo-cn20k-") as directory:
    root = Path(directory)
    makefile = root / "drivers/net/ethernet/marvell/octeontx2/af/Makefile"
    makefile.parent.mkdir(parents=True)
    makefile.write_bytes(original)
    def objects(enabled):
        result = subprocess.check_output([
            "make", "--no-print-directory", "-s", "-f", str(makefile), "-f", "-",
            "CONFIG_DEBUG_FS=" + enabled, "print-objects",
        ], input="print-objects:\n\t@printf '%s\\n' '$(rvu_af-y)'\n", text=True)
        return result.split()
    before = objects("")
    assert "cn20k/debugfs.o" in before  # Reproduces the broken disabled case.
    assert restore.apply_source_corrections(root, patch.parent, {}) == []
    assert makefile.read_bytes() == original
    bad = {'source_corrections': [{'name': patch.name, 'sha256': '0' * 64}]}
    try:
        restore.apply_source_corrections(root, patch.parent, bad)
    except ValueError:
        pass
    else:
        raise AssertionError("wrong patch bytes were accepted")
    assert makefile.read_bytes() == original
    assert restore.apply_source_corrections(root, patch.parent, approval) == approval['source_corrections']
    assert objects("") == [obj for obj in before if obj != "cn20k/debugfs.o"]
    enabled = objects("y")
    assert len(enabled) == len(set(enabled))
    assert set(enabled) == set(before)  # Every hardware/debug object retained.
    assert "cn20k/npc.o" in objects("")
    corrected = makefile.read_bytes()
    try:
        restore.apply_source_corrections(root, patch.parent, approval)
    except subprocess.CalledProcessError:
        pass
    else:
        raise AssertionError("already applied patch replay was accepted")
    assert makefile.read_bytes() == corrected
print("PASS: CONFIG_DEBUG_FS=n omits only CN20K debugfs; y retains all driver objects")

# Exercise staging from the independently pinned helper checkout, including
# the patch bytes that the read-only recovery context will contain.
sys.modules['kernel_checkpoint_restore'] = restore
stage_spec = importlib.util.spec_from_file_location('stage', kernel.parent / 'dev/stage-kernel-checkpoint.py')
stage = importlib.util.module_from_spec(stage_spec)
stage_spec.loader.exec_module(stage)
with tempfile.TemporaryDirectory(prefix='tilo-patch-staging-') as directory:
    root = Path(directory)
    source = root / 'public'; source.mkdir()
    record = dict(approval, purpose='tilo-kbuild-reconstruction-admission',
                  source_revision=restore.SOURCE,
                  checkpoint_reference='registry.tilo.so/tilo/tilo-development-partial-kernel-aarch64@sha256:' + '1' * 64,
                  restore_helper_sha256=restore.sha(restore.__file__))
    for field in ('writer_mapping_complete', 'dependency_invalidation_reviewed',
                  'toolchain_identity_required', 'excluded_selftests_not_build_inputs'):
        record[field] = True
    for filename, field in [('tree.tar', 'archive_sha256'), ('scan.json', 'scan_sha256'),
                            ('capture-receipt.json', 'capture_receipt_sha256'),
                            ('toolchain.json', 'toolchain_evidence_sha256')]:
        (source / filename).write_bytes(b'independently hashed fixture')
        record[field] = restore.sha(source / filename)
    payload = json.dumps(record).encode()
    stage.stage(source, root / 'context', payload, hashlib.sha256(payload).hexdigest(), record['checkpoint_reference'])
    assert (root / 'context/source-corrections' / patch.name).read_bytes() == patch.read_bytes()
    record['source_corrections'] = [{'name': patch.name, 'sha256': '0' * 64}]
    payload = json.dumps(record).encode()
    try:
        stage.stage(source, root / 'bad-context', payload, hashlib.sha256(payload).hexdigest(), record['checkpoint_reference'])
    except ValueError:
        pass
    else:
        raise AssertionError('staging accepted unapproved correction bytes')
    assert not (root / 'bad-context').exists()
print('PASS: recovery stages exact approved patch bytes and rejects mismatches before staging')
