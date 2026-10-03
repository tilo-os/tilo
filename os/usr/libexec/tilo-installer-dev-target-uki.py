#!/usr/bin/env python3
"""Project one exact CI development deployment into an unsigned target UKI.

Only the disposable installer VM admits this route. The OCI image and installed
composefs tree remain unchanged; the ESP entry and origin boot representation
use installed root discovery, retaining bootc's SHA256(kernel + initrd) identity.
"""
import argparse
import configparser
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

RECEIPT = Path('/run/tilo-installer/offline-target.json')
MARKER = Path('/usr/share/tilo/unverified-vm-install')
FLAVOUR = Path('/usr/lib/tilo/image-flavour')
PORT = Path('/dev/virtio-ports/org.tilo.vm-install-proof')
MOUNT = 'systemd.mount-extra=LABEL=TILO_BOOT:/boot:ext4:ro,nosuid,nodev,noexec'
PROFILE_TOOLS = Path('/usr/lib/tilo')
UKI_TOOL = Path('/usr/libexec/tilo-uki-cmdline')
HEX = re.compile(r'[0-9a-f]{128}\Z')


def require(ok, message):
    if not ok:
        raise ValueError(message)


def command(argv):
    result = subprocess.run([str(item) for item in argv], capture_output=True, text=True, timeout=180)
    require(result.returncode == 0, f'{Path(argv[0]).name} failed (exit {result.returncode}): '
            + (result.stderr.strip()[:1024] or 'no diagnostic stderr'))
    return result.stdout.strip()


def regular(root, relative):
    root = root.resolve(strict=True)
    path = root
    for part in Path(relative).parts:
        require(part not in ('', '.', '..', '/'), 'unsafe target boot path')
        path = path / part
        require(not path.is_symlink(), 'target boot path contains a symlink')
    require(path.is_file() and path.stat().st_size > 0, f'missing target boot input: {relative}')
    return path


def load_module(path, name):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def gate():
    require(os.geteuid() == 0 and MARKER.is_file() and not MARKER.is_symlink()
            and FLAVOUR.is_file() and not FLAVOUR.is_symlink() and FLAVOUR.read_bytes() == b'dev\n'
            and PORT.is_char_device(), 'unsigned target UKI requires the development installer VM')
    command(['systemd-detect-virt', '--vm', '--quiet'])
    require(os.environ.get('REQUIRE_TPM_PREDICTION', '0') == '0',
            'unsigned target UKI cannot provide TPM prediction')
    require(RECEIPT.is_file() and not RECEIPT.is_symlink() and RECEIPT.stat().st_size <= 8192,
            'development target receipt missing or unsafe')
    selected = json.loads(RECEIPT.read_text())
    require(selected.get('schema') == 'tilo.installer-dev-target.v1'
            and re.fullmatch(r'sha256:[0-9a-f]{64}', selected.get('target_digest', ''))
            and selected.get('target_ref') == 'registry.tilo.so/tilo/tilo-vm@' + selected['target_digest'],
            'unsigned UKI requires the verified development target')
    return selected


def image_inputs(root):
    require(regular(root, 'usr/lib/tilo/image-flavour').read_bytes() == b'dev\n',
            'target is not development flavour')
    modules = root / 'usr/lib/modules'
    versions = [path for path in modules.iterdir() if path.is_dir() and not path.is_symlink()]
    require(len(versions) == 1 and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._+-]*', versions[0].name),
            'development target requires exactly one kernel')
    version = versions[0].name
    kernel_directory = f'usr/lib/modules/{version}'
    if not (root / kernel_directory / 'vmlinuz').exists() and not (root / kernel_directory / 'initramfs.img').exists():
        kernel_directory = f'kernel/{version}'
    kernel = regular(root, kernel_directory + '/vmlinuz')
    initrd = regular(root, kernel_directory + '/initramfs.img')
    require(not (root / 'boot/tilo-target').exists(), 'development target already has a target boot bundle')
    sys.path.insert(0, str(PROFILE_TOOLS))
    profile = load_module(PROFILE_TOOLS / 'hardware_profile.py', 'dev_target_profile')
    dtb = profile.resolve_profile(root, version, 'aarch64')
    uki_paths = list((root / 'boot/EFI/Linux').glob('*.efi'))
    ukis = [regular(root, path.relative_to(root)) for path in uki_paths]
    require(len(ukis) <= 1, 'development target has ambiguous UKIs')
    require(not kernel_directory.startswith('kernel/') or len(ukis) == 1,
            'split development target requires one nonempty sealed UKI')
    if ukis:
        actual = sections(ukis[0])
        require(actual.get('.linux') == kernel.read_bytes() and actual.get('.initrd') == initrd.read_bytes(),
                'CI development UKI differs from its split kernel payload')
        require((dtb is None and '.dtb' not in actual)
                or (dtb is not None and actual.get('.dtb') == dtb.read_bytes()),
                'CI development UKI differs from its pinned hardware DTB')
    return version, kernel, initrd, dtb


def fields(raw):
    values = {}
    for line in raw.splitlines():
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        key, separator, value = line.strip().partition(' ')
        require(separator and key not in values, 'ambiguous installed BLS entry')
        values[key] = value.strip()
    require(all(key in values for key in ('linux', 'initrd', 'options'))
            and not {'efi', 'uki'} & values.keys(), 'expected one split-kernel BLS entry')
    return values


def sections(path):
    tool = load_module(UKI_TOOL, 'dev_target_sections')
    data = path.read_bytes()
    result = {}
    for name, offset, raw_size, size in tool.sections(data):
        if name not in ('.linux', '.initrd', '.cmdline', '.dtb'):
            continue
        require(name not in result and 0 < size <= raw_size and offset + size <= len(data),
                'invalid generated UKI section')
        result[name] = data[offset:offset + size]
    require(int.from_bytes(data[int.from_bytes(data[60:64], 'little') + 4:][:2], 'little') == 0xaa64,
            'generated target UKI is not ARM64')
    return result


def promote(image_root, target_root, esp, selected):
    version, kernel, initrd, dtb = image_inputs(image_root)
    deployments = list((target_root / 'state/deploy').glob('*/etc'))
    require(len(deployments) == 1 and HEX.fullmatch(deployments[0].parent.name),
            'development target requires one installed composefs deployment')
    digest = deployments[0].parent.name
    origin = regular(target_root, f'state/deploy/{digest}/{digest}.origin')
    ini = configparser.ConfigParser(interpolation=None, strict=True)
    ini.read_string(origin.read_text())
    require(ini.get('image', 'manifest_digest') == selected['target_digest']
            and ini.get('origin', 'container-image-reference') ==
            'ostree-unverified-image:docker://' + selected['update_origin']
            and ini.get('boot', 'boot_type') in ('bls', 'uki'),
            'installed origin differs from the selected CI development image')
    entries = list((esp / 'loader/entries').glob('*.conf'))
    require(len(entries) == 1, 'expected exactly one installed BLS entry')
    entry = regular(esp, entries[0].relative_to(esp))
    boot_type = ini.get('boot', 'boot_type')
    kernel_bytes, initrd_bytes = kernel.read_bytes(), initrd.read_bytes()
    relative = f'EFI/Linux/bootc/bootc_composefs-{digest}.efi'
    destination = esp / relative
    if boot_type == 'bls':
        bls = fields(entry.read_text())
        installed_kernel = regular(esp, bls['linux'].lstrip('/'))
        installed_initrd = regular(esp, bls['initrd'].lstrip('/'))
        require(installed_kernel.read_bytes() == kernel_bytes and installed_initrd.read_bytes() == initrd_bytes,
                'installed boot payload differs from the selected CI development image')
        words = bls['options'].split()
        require(not destination.exists() and not destination.is_symlink(), 'target UKI already exists')
    else:
        installed = sections(regular(esp, relative))
        require(installed.get('.linux') == kernel_bytes and installed.get('.initrd') == initrd_bytes,
                'installed UKI differs from the selected CI development image')
        require((dtb is None and '.dtb' not in installed)
                or (dtb is not None and installed.get('.dtb') == dtb.read_bytes()),
                'installed UKI differs from immutable hardware DTB')
        words = installed.get('.cmdline', b'').rstrip(b'\0\n').decode().split()
        values = {}
        for line in entry.read_text().splitlines():
            key, separator, value = line.partition(' ')
            if separator:
                require(key not in values, 'ambiguous installed UKI loader entry')
                values[key] = value.strip()
        require(values.get('efi', values.get('uki')) == '/' + relative
                and not {'linux', 'initrd', 'options'} & values.keys(),
                'installed loader entry selects a different UKI')
        installed_kernel, installed_initrd = kernel, initrd
    boot_digest = hashlib.sha256(kernel_bytes + initrd_bytes).hexdigest()
    require(ini.get('boot', 'digest') == boot_digest, 'installed bootc payload digest differs')
    require([word for word in words if word.startswith('composefs=')] == ['composefs=' + digest]
            and [word for word in words if word.startswith('root=')] == ['root=tilo-auto'],
            'installed boot entry has a different root or composefs identity')
    boot_mounts = [word for word in words if word.startswith('systemd.mount-extra=')
                   and len(word.split(':')) >= 2 and word.split(':')[1] == '/boot']
    require(len(boot_mounts) <= 1 and all(word == MOUNT for word in boot_mounts),
            'installed boot entry has a conflicting /boot mount')
    words = [word for word in words if word != 'tilo.install']
    if not boot_mounts:
        words.append(MOUNT)
    cmdline = ' '.join(words)
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Only private scratch and final ESP bytes are written. No image commit,
    # tag, or rootfs layer is created, and no target identity fields are altered.
    with tempfile.TemporaryDirectory(prefix='tilo-dev-target-', dir=esp) as temporary:
        work = Path(temporary)
        (work / 'cmdline').write_text(cmdline + '\n')
        output = work / 'target.efi'
        arguments = ['ukify', 'build', '--linux', installed_kernel, '--initrd', installed_initrd,
                     '--stub', regular(image_root, 'usr/lib/systemd/boot/efi/linuxaa64.efi.stub'),
                     '--uname', version, '--os-release', '@' + str(regular(image_root, 'usr/lib/os-release')),
                     '--cmdline', '@' + str(work / 'cmdline'), '--output', output]
        if dtb is not None:
            arguments += ['--devicetree', dtb]
        command(arguments)
        actual = sections(output)
        require(actual.get('.linux') == kernel_bytes and actual.get('.initrd') == initrd_bytes
                and actual.get('.cmdline', b'').rstrip(b'\0\n') == cmdline.encode(),
                'generated target UKI changed kernel, initramfs or command line')
        require((dtb is None and '.dtb' not in actual)
                or (dtb is not None and actual.get('.dtb') == dtb.read_bytes()),
                'generated target UKI differs from the immutable hardware DTB')
        output.chmod(0o644)
        output.replace(destination)
    if boot_type == 'bls':
        kept = [line for line in entry.read_text().splitlines()
                if line.partition(' ')[0] not in ('linux', 'initrd', 'options')]
        new_entry = entry.with_suffix('.conf.tmp')
        new_entry.write_text('\n'.join(kept) + '\nefi /' + relative + '\n')
        new_entry.chmod(0o644)
        new_entry.replace(entry)
        # bootc 1.16.4 boot.rs hashes identical kernel+initrd bytes for bls and uki.
        # Transition only the representation; retain manifest, origin and digest.
        ini.set('boot', 'boot_type', 'uki')
        buffer = io.StringIO()
        ini.write(buffer)
        temporary_origin = origin.with_suffix('.origin.tmp')
        temporary_origin.write_text(buffer.getvalue())
        info = origin.stat()
        temporary_origin.chmod(info.st_mode & 0o777)
        os.chown(temporary_origin, info.st_uid, info.st_gid)
        for name in os.listxattr(origin):
            os.setxattr(temporary_origin, name, os.getxattr(origin, name))
        temporary_origin.replace(origin)
    return digest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('validate-image', 'promote'))
    parser.add_argument('--image-root', type=Path, required=True)
    parser.add_argument('--target-root', type=Path)
    parser.add_argument('--esp', type=Path)
    args = parser.parse_args()
    try:
        selected = gate()
        if args.command == 'validate-image':
            image_inputs(args.image_root)
        else:
            require(args.target_root is not None and args.esp is not None, 'target root and ESP required')
            print(promote(args.image_root, args.target_root, args.esp, selected))
    except (ValueError, OSError, KeyError, configparser.Error, subprocess.SubprocessError) as error:
        parser.exit(1, f'development target UKI: {error}\n')


if __name__ == '__main__':
    main()
