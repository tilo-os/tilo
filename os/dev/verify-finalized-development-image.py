#!/usr/bin/python3
"""Verify one finalized local image using pinned coherent tools; never install."""
import argparse
import importlib.util
import json
from pathlib import Path
import sys


def verify(args):
    dev = args.helpers / 'os/dev'
    sys.path.insert(0, str(dev))
    def module(name, filename):
        spec = importlib.util.spec_from_file_location(name, dev / filename)
        value = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(value)
        return value
    rootfs = module('finalization_check_rootfs', 'sign-development-rootfs.py')
    prediction = module('finalization_check_prediction', 'development_prediction.py')
    audit = module('finalization_check_layers', 'audit-rootfs-layers.py')
    inspector = module('finalization_check_selection', 'verify-installed-development-disk.py')
    with rootfs.mounted(args.image_id) as root:
        profile = rootfs.source.mounted_identity(root, args.target, args.manifest_sha256, args.architecture)
        proof = prediction.verify_prediction(root, args.public_bundle, args.manifest_sha256,
            args.architecture, args.generation, args.composefs_digest, args.systemd_version)
        selection = None
        if getattr(args, 'boot_profile', 'installed-target') == 'installer-media':
            paths = [row['path'] for row in proof['signed_ukis'] if row['path'].startswith('boot/EFI/Linux/')]
            if len(paths) != 1:
                raise ValueError('installer requires exactly one authenticated live UKI')
            selection = inspector.source_boot_selection(root, proof, 'installer-media', paths[0],
                args.architecture, args.generation, args.composefs_digest)
    material = audit.audit_image(args.image_id)
    if material['status'] != 'passed-scoped-scan':
        raise ValueError('finalized image layer audit did not pass')
    return {'hardware_profile': profile, 'selected_boot': selection, 'prediction': proof, 'layer_material_audit': material,
            'installable': False, 'registry_signature_verified': False,
            'installed_bytes_verified': False, 'vm_boot_verified': False}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--helpers', type=Path, required=True)
    parser.add_argument('--public-bundle', type=Path, required=True)
    parser.add_argument('--generation', type=int, required=True)
    parser.add_argument('--boot-profile', choices=('installed-target','installer-media'), default='installed-target')
    for name in ('image-id', 'target', 'manifest-sha256', 'architecture', 'composefs-digest', 'systemd-version'):
        parser.add_argument('--' + name, required=True)
    try:
        print(json.dumps(verify(parser.parse_args()), sort_keys=True))
    except (OSError, ValueError, TypeError, KeyError):
        parser.exit(1, 'finalized image verification failed; no private diagnostics emitted\n')
