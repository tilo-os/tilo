#!/usr/bin/env python3
"""Make a new, receipted runtime from verified inputs; never mutate the base."""
import argparse
import json
from pathlib import Path
import shutil
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qualify_apk import ARCHES, digest, elf_machine, pinned, runtime_files, verify_runtime


def stage(args):
    base = verify_runtime(args.base, args.base_receipt, args.base_receipt_sha256, args.architecture)
    for path, sha in [(args.framework, args.framework_sha256), (args.bridge, args.bridge_sha256),
                      (args.entry, args.entry_sha256)]:
        pinned(path, sha)
    for path in (args.bridge, args.entry):
        if elf_machine(path.read_bytes()[:20]) != ARCHES[args.architecture][1]:
            raise ValueError('mediation ELF architecture mismatch')
    if args.output.exists() or args.receipt.exists():
        raise ValueError('refusing existing output')
    if args.receipt.resolve().is_relative_to(args.output.resolve()) or args.output.resolve().is_relative_to(args.base.resolve()):
        raise ValueError('outputs must be separate from inputs and each other')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=args.output.parent, prefix='.android-mediation-') as temp:
        root = Path(temp) / 'runtime'
        shutil.copytree(args.base, root)
        verify_runtime(root, args.base_receipt, args.base_receipt_sha256, args.architecture)
        replacements = [(args.framework, 'usr/lib/java/dex/android_translation_layer/api-impl.jar'),
                        (args.bridge, 'lib/libtilo-android-chooser.so'),
                        (args.entry, 'bin/android-translation-layer')]
        for source, target in replacements:
            shutil.copyfile(source, root / target)
            (root / target).chmod(0o755 if target.startswith('bin/') else 0o644)
            expected = {args.framework: args.framework_sha256, args.bridge: args.bridge_sha256,
                        args.entry: args.entry_sha256}[source]
            pinned(root / target, expected)
        provenance = {'upstream_commit': 'f4622e36186f5757b75a605deff543145df3094b',
                      'upstream_license': 'GPL-3.0-only', 'base_receipt_sha256': args.base_receipt_sha256,
                      'framework_sha256': args.framework_sha256, 'bridge_sha256': args.bridge_sha256,
                      'entry_sha256': args.entry_sha256, 'execution_accepted': False}
        (root / 'licenses/tilo-mediation.json').write_text(json.dumps(provenance, indent=2) + '\n')
        receipt = {**base, 'entry_sha256': args.entry_sha256, 'mediation': provenance, 'files': runtime_files(root)}
        args.receipt.parent.mkdir(parents=True, exist_ok=True)
        args.receipt.write_text(json.dumps(receipt, indent=2, sort_keys=True) + '\n')
        root.rename(args.output)
    print(json.dumps({'runtime_root': str(args.output), 'receipt_sha256': digest(args.receipt)}))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('base', 'base-receipt', 'framework', 'bridge', 'entry', 'output', 'receipt'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('base-receipt', 'framework', 'bridge', 'entry'):
        parser.add_argument('--' + name + '-sha256', required=True)
    parser.add_argument('--architecture', choices=ARCHES, required=True)
    stage(parser.parse_args())
