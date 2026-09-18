#!/usr/bin/python3
"""Materialize authenticated target boot artifacts on a mounted installer ESP.

bootc 1.16.4 installs recognized Type2 UKIs, not arbitrary /boot files. Keep
these sidecars on the ESP, outside the sealed composefs root, to avoid making a
UKI's composefs digest depend on its own bytes. The caller owns and confines the
ESP mount and holds the independently verified source mount for this call.
"""
import hashlib
import os
from pathlib import Path
import shutil
import tempfile

import development_prediction as prediction


def materialize(root, esp, public_bundle, manifest_sha256, architecture,
                generation, composefs_digest, systemd_version):
    proof = prediction.verify_prediction(root, public_bundle, manifest_sha256,
        architecture, generation, composefs_digest, systemd_version)
    root, esp = Path(root), Path(esp)
    if not esp.is_absolute() or esp.resolve() != esp or not esp.is_dir():
        raise ValueError('a canonical mounted ESP directory is required')
    destination = esp / 'tilo-target'
    if destination.exists() or destination.is_symlink():
        raise ValueError('target metadata destination already exists')
    relatives = ('tilo-target.efi', f'predictions/generation-{generation}.json',
                 f'predictions/generation-{generation}.json.uki.sig')
    limits = (prediction.boot.UKI_LIMIT, 16384, 1024)
    # Snapshot all verified source bytes before writing the destination. The
    # caller must retain the immutable source mount throughout this operation;
    # arbitrary mutable directories are not an authenticated input.
    with tempfile.TemporaryDirectory(prefix='tilo-target-materialize-') as temporary:
        snapshots = Path(temporary)
        rows = []
        for relative, limit in zip(relatives, limits):
            source = prediction.boot.hardware.trusted_path(
                root, Path('boot/tilo-target') / relative, regular=True)
            output = snapshots / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            data = prediction.boot.loader.snapshot(source, output, limit)
            rows.append({'path': 'tilo-target/' + relative,
                         'sha256': hashlib.sha256(data).hexdigest(), 'size': len(data)})
        if rows[0]['sha256'] != proof['target_uki_sha256']:
            raise ValueError('target changed after verification')
        # The source is a held immutable image mount; record exact sidecar bytes
        # for the independent installed-media verifier to compare after copying.
        destination.mkdir(mode=0o755)
        try:
            for relative in relatives:
                target = destination / relative
                target.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
                with open(snapshots / relative, 'rb') as source, open(target, 'xb') as output:
                    shutil.copyfileobj(source, output)
                    output.flush()
                    os.fsync(output.fileno())
                target.chmod(0o644)
            for row in rows:
                if hashlib.sha256((esp / row['path']).read_bytes()).hexdigest() != row['sha256']:
                    raise ValueError('materialized target bytes differ')
        except BaseException:
            shutil.rmtree(destination)
            raise
    return {'schema': 1, 'purpose': 'tilo-installer-target-materialization',
            'files': rows, 'source_prediction': proof,
            'installed_media_verified': False, 'boot_verified': False}
