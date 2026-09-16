#!/usr/bin/env python3
"""Merge two validated traditional Wine artifacts without dropping subpackages.

Architecture-specific outputs are all retained. Shared noarch packages must
have equal complete semantic RPM headers (including file digests and scripts).
Only container/compression/signature/build-host bookkeeping is ignored. The
x86_64 build owns the shared noarch bytes after successful comparison.
"""
import argparse
import json
from pathlib import Path
import shutil
import xml.etree.ElementTree as ET

import wine_rpm_contract as contract

# Differences here describe RPM encoding/signing/build-machine bookkeeping,
# never installed contents, dependencies, scripts, triggers, ownership or flags.
ENCODING_TAGS = frozenset('''
SIGSIZE SIGMD5 SIGPGP SIGGPG RSAHEADER DSAHEADER SHA1HEADER SHA256HEADER
LONGSIGSIZE LONGARCHIVESIZE ARCHIVESIZE PAYLOADDIGEST PAYLOADDIGESTALT
PAYLOADSHA256 PAYLOADSHA512 PAYLOADSHA3_256 PAYLOADDIGESTALGO PAYLOADSIZE
PAYLOADFORMAT PAYLOADCOMPRESSOR PAYLOADFLAGS HEADERIMMUTABLE HEADERREGIONS
BUILDTIME BUILDHOST COOKIE RPMVERSION
'''.split())


def semantic_header(xml):
    root = ET.fromstring(xml)
    if root.tag != 'rpmHeader':
        raise ValueError('expected one RPM XML header')
    result = {}
    for tag in root:
        name = tag.attrib['name'].upper()
        if name in ENCODING_TAGS:
            continue
        if name in result:
            raise ValueError('duplicate semantic RPM tag')
        result[name] = [(child.tag, child.text or '') for child in tag]
    for required in ('NAME', 'VERSION', 'RELEASE', 'ARCH', 'FILENAMES', 'FILEDIGESTS'):
        if required not in result:
            raise ValueError(f'noarch header is missing {required}')
    if result['ARCH'] != [('string', 'noarch')]:
        raise ValueError('shared package must be noarch')
    return result


def noarch_equal(left, right):
    # Verify payload/header digests even for the locally built unsigned RPMs.
    # This is not a claim that the output already has a publication signature.
    for path in (left, right):
        contract.run(['rpmkeys', '--checksig', '--nosignature', str(path)])
    a = semantic_header(contract.run(['rpm', '-qp', '--xml', str(left)]))
    b = semantic_header(contract.run(['rpm', '-qp', '--xml', str(right)]))
    if a != b:
        differing = sorted(name for name in a.keys() | b.keys() if a.get(name) != b.get(name))
        raise ValueError('noarch output differs: ' + ', '.join(differing))


def select_packages(roots, inventories, compare=noarch_equal):
    shared = [{name for name, data in inventory.items() if data['identity'][3] == 'noarch'}
              for inventory in inventories]
    if shared[0] != shared[1]:
        raise ValueError('paired builds must produce the same complete noarch package set')
    selected = {}
    for index, (root, inventory) in enumerate(zip(roots, inventories, strict=True)):
        for name, data in inventory.items():
            if Path(name).name != name:
                raise ValueError('invalid RPM filename')
            if name in selected:
                if name not in shared[0]:
                    raise ValueError('architecture-specific output filename collision')
                compare(roots[0] / 'rpms' / name, root / 'rpms' / name)
            else:
                selected[name] = {'owner': 'x86_64' if index == 0 else 'i686', **data}
    return selected


def merge(x64, x86, destination, pin, refs):
    roots = [Path(x64), Path(x86)]
    records = []
    for root, architecture, ref in zip(roots, ('x86_64', 'i686'), refs, strict=True):
        contract.reference(ref)
        stored = json.loads((root / 'provenance/contract.json').read_text())
        fresh = contract.output(architecture, pin, stored['buildroot_ref'], root, write=False)
        if fresh != stored:
            raise ValueError('input artifact does not match its recorded package contract')
        if any(path.is_symlink() or not (path.is_file() or path.is_dir())
               for path in (root / 'provenance').rglob('*')):
            raise ValueError('provenance must contain only regular files and directories')
        records.append(fresh)
    selected = select_packages(roots, [record['rpms'] for record in records])
    destination = Path(destination)
    if destination.exists() or destination.is_symlink():
        raise ValueError('merge destination must not exist')
    # All input checks finish before writing any output.
    (destination / 'rpms').mkdir(parents=True)
    for name, data in selected.items():
        index = 0 if data['owner'] == 'x86_64' else 1
        shutil.copyfile(roots[index] / 'rpms' / name, destination / 'rpms' / name)
    for root, architecture in zip(roots, ('x86_64', 'i686'), strict=True):
        shutil.copytree(root / 'provenance', destination / 'provenance' / architecture)
    result = {'schema': 1, 'input_refs': dict(zip(('x86_64', 'i686'), refs, strict=True)),
              'noarch_owner': 'x86_64', 'source': pin, 'rpms': selected,
              'execution_validated': False}
    (destination / 'paired-contract.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('x64', 'x86', 'output', 'pin', 'x64-ref', 'x86-ref'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    merge(args.x64, args.x86, args.output, json.loads(Path(args.pin).read_text()),
          [args.x64_ref, args.x86_ref])
