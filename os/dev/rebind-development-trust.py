"""Replace public development trust in an explicitly disposable raw filesystem.

Not an image importer or completion operation: initramfs regeneration is mandatory
before this filesystem can proceed to loader signing and sealing.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import tempfile

HERE = Path(__file__).resolve().parent

def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded

installer = module('rebind_installer', 'install-public-trust.py')
retention = module('rebind_retention', 'retain-development-rootfs.py')
hardware = retention.hardware


def replace(image_root, *, disposable, old_bundle, old_manifest, old_target,
            new_bundle, new_manifest, new_target, new_owner, architecture, generation):
    root = Path(image_root)
    if (disposable is not True or not root.is_absolute() or root.resolve() != root
            or root == Path('/') or os.getuid() != 0 or architecture != platform.machine()
            or architecture not in retention.source.ARCHITECTURES
            or type(generation) is not int or not 1 <= generation < 2**64):
        raise ValueError('explicit disposable native root filesystem required')
    hardware.trusted_path(root, Path(), regular=False)
    hardware.development_target(new_target, architecture)
    # Snapshot both independently pinned public bundles before any mutation.
    with tempfile.TemporaryDirectory(prefix='tilo-public-rebind-') as temporary:
        work = Path(temporary)
        bundles = (('old', Path(old_bundle), old_manifest), ('new', Path(new_bundle), new_manifest))
        for _, source, digest in bundles:
            installer.trust.validate(source, digest)
        for name, source, digest in bundles:
            snapshot = work / name
            snapshot.mkdir()
            for filename in (*installer.trust.FILES, 'manifest.json'):
                (snapshot / filename).write_bytes(installer.trust.read_public(source / filename))
            installer.trust.validate(snapshot, digest)
        retention.source.mounted_identity(root, old_target, old_manifest, architecture)
        profile = hardware.read_profile(root)
        kver = retention.raw_kernel_pair(root)
        generation_path = hardware.trusted_path(root, Path('usr/lib/tilo/generation'), regular=True)
        if generation_path.stat().st_size > 32:
            raise ValueError('raw generation exceeds size bound')
        if generation_path.read_bytes() not in (str(generation).encode(), f'{generation}\n'.encode()):
            raise ValueError('raw generation differs')
        for relative in ('boot/EFI/Linux', 'boot/tilo-target', 'kernel'):
            path = root / relative
            if path.exists() or path.is_symlink():
                raise ValueError('completed or split rootfs is forbidden')
        loader_dir = hardware.trusted_path(root, Path('usr/lib/systemd/boot/efi'), regular=False)
        if any(path.name.endswith('.signed') for path in loader_dir.iterdir()):
            raise ValueError('signed rootfs is forbidden')
        # Generate the complete replacement elsewhere; install() retains its
        # create-only contract and all new-owner/origin checks run before writes.
        staged = work / 'staged'
        installer.install(work / 'new', work / 'new', staged / 'usr/share/tilo',
            'development', new_manifest, new_owner, new_target, staged)
        replacements = {path.relative_to(staged): path.read_bytes()
                        for path in staged.rglob('*') if path.is_file()}
        if profile is not None:
            profile = profile | {'target_update_ref': new_target}
            replacements[hardware.PROFILE_PATH] = (json.dumps(profile, sort_keys=True, indent=2) + '\n').encode()
        # Every destination must already be a checked immutable regular file.
        # No directories, unknown files, redirects, or production markers are
        # removed. Fail all validation before touching the disposable root.
        destinations = {relative: hardware.trusted_path(root, relative, regular=True)
                        for relative in replacements}
        preserved = [generation_path, root / 'usr/lib/modules' / kver / 'vmlinuz',
                     root / 'usr/lib/modules' / kver / 'initramfs.img']
        before = {}
        for path in preserved:
            with path.open('rb') as stream:
                before[path] = hashlib.file_digest(stream, 'sha256').hexdigest()
        pending = root / hardware.DEVELOPMENT_PATH / 'initramfs-rebind-required.json'
        # Mark before the first mutation. Every source/signing/retention consumer
        # rejects this state until a separate regeneration verifier clears it.
        marker = {'schema': 1, 'purpose': 'tilo-development-initramfs-rebind-required',
                  'kernel_version': kver, 'generation': generation, 'architecture': architecture,
                  'old_manifest_sha256': old_manifest, 'new_manifest_sha256': new_manifest,
                  'old_target_update_ref': old_target, 'new_target_update_ref': new_target,
                  'original_kernel_sha256': before[preserved[1]],
                  'original_initramfs_sha256': before[preserved[2]],
                  'old_pcr_public_sha256': hashlib.sha256(installer.trust.read_public(
                      work / 'old/tpm2-pcr-signing.public.pem')).hexdigest(),
                  'new_pcr_public_sha256': hashlib.sha256(installer.trust.read_public(
                      work / 'new/tpm2-pcr-signing.public.pem')).hexdigest()}
        with pending.open('x') as output:
            json.dump(marker, output, sort_keys=True)
        pending.chmod(0o644)
        for relative, path in destinations.items():
            # Replace directory entries rather than following existing hardlinks.
            descriptor, temporary_path = tempfile.mkstemp(prefix='.tilo-public-', dir=path.parent)
            try:
                with os.fdopen(descriptor, 'wb') as output:
                    output.write(replacements[relative])
                os.chmod(temporary_path, 0o644)
                os.replace(temporary_path, path)
            finally:
                Path(temporary_path).unlink(missing_ok=True)
        # Check replacement consistency only. Do not call or bypass the shared
        # acceptance gate: this deliberately remains a rejected pending root.
        if hardware.development_origin(root) != new_target:
            raise ValueError('replacement origin differs')
        updated_profile = hardware.read_profile(root)
        if updated_profile != profile:
            raise ValueError('replacement hardware profile differs')
        if retention.raw_kernel_pair(root) != kver:
            raise ValueError('kernel identity changed')
        for path, digest in before.items():
            with path.open('rb') as stream:
                if hashlib.file_digest(stream, 'sha256').hexdigest() != digest:
                    raise ValueError('preserved raw bytes changed')
        return {'schema': 1, 'purpose': 'development-public-trust-replacement',
                'old_manifest_sha256': old_manifest, 'new_manifest_sha256': new_manifest,
                'target_update_ref': new_target, 'kernel_version': kver, 'generation': generation,
                'installable': False, 'initramfs_regenerated': False,
                'initramfs_regeneration_required': True,
                'completion_required': ['regenerate initramfs', 'sign loader', 'seal and sign UKIs']}
