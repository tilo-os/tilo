#!/usr/bin/env python3
"""Stage independently admitted public kernel checkpoint for a single recovery."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import kernel_checkpoint_restore as restore
import kernel_failure_checkpoint as failure


def stage(source, destination, approval_bytes, approval_sha, checkpoint):
    if not re.fullmatch(r'ghcr\.io/tilo-os/tilo-development-partial-kernel-aarch64@sha256:[0-9a-f]{64}', checkpoint):
        raise ValueError('immutable partial-kernel reference required')
    if hashlib.sha256(approval_bytes).hexdigest() != approval_sha:
        raise ValueError('independent approval hash differs')
    approval = json.loads(approval_bytes)
    if approval.get('checkpoint_reference') != checkpoint:
        raise ValueError('approval does not bind checkpoint reference')
    if approval.get('purpose') != 'tilo-kbuild-reconstruction-admission' or approval.get('source_revision') != restore.SOURCE:
        raise ValueError('approval purpose/source differs')
    for field in ('writer_mapping_complete', 'dependency_invalidation_reviewed',
                  'toolchain_identity_required', 'excluded_selftests_not_build_inputs'):
        if approval.get(field) is not True:
            raise ValueError('unresolved admission: ' + field)
    helper = Path(restore.__file__)
    if restore.sha(helper) != approval['restore_helper_sha256']:
        raise ValueError('reviewed helper differs')
    files = {'tree.tar': 'archive_sha256', 'scan.json': 'scan_sha256',
             'capture-receipt.json': 'capture_receipt_sha256',
             'toolchain.json': 'toolchain_evidence_sha256'}
    for name, field in files.items():
        path = source / name
        if not path.is_file() or path.is_symlink() or restore.sha(path) != approval[field]:
            raise ValueError('checkpoint file differs: ' + name)
    corrections=restore.source_corrections(Path(__file__).resolve().parents[1]/'kernel/patches',approval)
    destination.mkdir()
    for name in files:
        # Same workspace filesystem: do not duplicate the large archive.
        os.link(source / name, destination / name)
    if corrections:
        (destination/'source-corrections').mkdir()
        for correction in corrections:
            shutil.copyfile(correction,destination/'source-corrections'/correction.name)
    shutil.copyfile(helper, destination / 'kernel_checkpoint_restore.py')
    shutil.copyfile(Path(failure.__file__), destination / 'kernel_failure_checkpoint.py')
    (destination / 'approval.json').write_bytes(approval_bytes)


if __name__ == '__main__':
    stage(Path('checkpoint-public'), Path('checkpoint-context'),
          os.environ['CHECKPOINT_APPROVAL'].encode(), os.environ['APPROVAL_SHA256'],
          os.environ['CHECKPOINT_REF'])
    original = Path('os/kernel/Containerfile').read_bytes()
    recipe = restore.derive(original, os.environ['APPROVAL_SHA256'])
    recipe = failure.derive(recipe)
    Path('kernel-recovery.Containerfile').write_bytes(recipe)
    # Named context alone supplies the checkpoint; keep its multi-GiB archive
    # out of the ordinary source context and exclude helper checkout metadata.
    with Path('kernel-recovery.Containerfile.dockerignore').open('x') as out:
        original_ignore = Path('.dockerignore')
        if original_ignore.exists():
            out.write(original_ignore.read_text())
        out.write('\ncheckpoint-public\ncheckpoint-context\nrecovery-helpers\nkernel-evidence\n')
    Path('kernel-evidence').mkdir(exist_ok=True)
    Path('kernel-evidence/reconstruction.json').write_text(json.dumps(dict(
        purpose='tilo-kernel-checkpoint-reconstruction', source_revision=restore.SOURCE,
        original_recipe_sha256=restore.RECIPE,
        derived_recipe_sha256=hashlib.sha256(recipe).hexdigest(),
        helper_revision=os.environ['HELPER_REVISION'],
        restore_helper_sha256=restore.sha(Path(restore.__file__)),
        failure_capture_helper_sha256=restore.sha(Path(failure.__file__)),
        failure_cache_identity=os.environ['GITHUB_RUN_ID']+'-'+os.environ['GITHUB_RUN_ATTEMPT'],
        checkpoint_reference=os.environ['CHECKPOINT_REF'],
        approval_sha256=os.environ['APPROVAL_SHA256'], kernel_build_jobs=4,
        admitted_input_sha256={key: json.loads(os.environ['CHECKPOINT_APPROVAL'])[key]
            for key in ('archive_sha256', 'scan_sha256', 'capture_receipt_sha256',
                        'toolchain_evidence_sha256')},
        original_recipe_executed_verbatim=False,
        toolchain_identity_check='required inside resumed build',
        fresh_module_signing_identity_required=True), sort_keys=True, indent=2) + '\n')
