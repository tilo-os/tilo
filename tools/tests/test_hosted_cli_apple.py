"""The hosted release requires one pinned Developer ID on the evidence and the signed programs."""
import hashlib
import importlib.util
import json
import sys
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('apple', ROOT / 'tools/ci/publish/hosted_cli_apple.py')
apple = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(apple)
sys.path.insert(0, str(Path(__file__).parent))
from test_macos_signing_macho import fixture

TEAM = 'TEAM123456'
# Apple's csreq -b for the operator's exact shared requirement, with just the team substituted.
REQUIREMENT = bytes.fromhex(
    'fade0c01000000b4000000010000000300000014fade0c00000000a0000000010000000600000006'
    '00000006000000060000000200000012736f2e74696c6f2e636c692e686f7374656400000000000f'
    '0000000e000000010000000a2a864886f763640602060000000000000000000e000000000000000a'
    '2a864886f7636406010d0000000000000000000b000000000000000a7375626a6563742e4f550000'
    '000000010000000a') + TEAM.encode() + bytes(2)


def openssl(*args, data=None):
    return subprocess.run(['openssl', *map(str, args)], input=data, capture_output=True, check=True).stdout


def certificate(work, name, parent=None, *, ca=False, marker=None):
    key, csr, crt = (work / f'{name}.{ext}' for ext in ('key', 'csr', 'pem'))
    openssl('req', '-new', '-newkey', 'rsa:2048', '-nodes', '-subj', f'/CN={name}/OU={TEAM}',
            '-keyout', key, '-out', csr)
    extensions = work / 'extensions'
    extensions.write_text(('basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n'
                           if ca else 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=codeSigning\n')
                          + (f'{marker}=critical,DER:05:00\n' if marker else ''))
    signer = ['-signkey', key] if not parent else ['-CA', parent[0], '-CAkey', parent[1], '-CAcreateserial']
    openssl('x509', '-req', '-in', csr, '-days', '2', '-sha256', *signer, '-extfile', extensions, '-out', crt)
    return crt, key


def der_hash(cert):
    return hashlib.sha256(openssl('x509', '-in', cert, '-outform', 'DER')).hexdigest()


def code_directory(image):
    ident = b'so.tilo.cli.hosted\0'
    names = ident + TEAM.encode() + b'\0'
    special = hashlib.sha256(REQUIREMENT).digest() + bytes(32)
    hashes = b''.join(hashlib.sha256(image[i:i + 4096]).digest() for i in range(0, len(image), 4096))
    offset = 96 + len(names) + len(special)
    cd = bytearray(96)
    struct.pack_into('>9I4B', cd, 0, 0xfade0c02, offset + len(hashes), 0x20500, 0x10000, offset,
                     96, 2, len(hashes) // 32, len(image), 32, 2, 0, 12)
    struct.pack_into('>I', cd, 48, 96 + len(ident))
    struct.pack_into('>Q', cd, 80, 1)
    return bytes(cd) + names + special + hashes


class SignatureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.temp.cleanup)
        cls.work = Path(cls.temp.name)
        cls.root = certificate(cls.work, 'root', ca=True)
        cls.issuer = certificate(cls.work, 'issuer', cls.root, ca=True, marker='1.2.840.113635.100.6.2.6')
        cls.leaf = certificate(cls.work, 'Developer ID Application: fixture', cls.issuer, marker='1.2.840.113635.100.6.1.13')
        cls.other = certificate(cls.work, 'Developer ID Application: second', cls.issuer, marker='1.2.840.113635.100.6.1.13')
        cls.unmarked = certificate(cls.work, 'Developer ID Application: ordinary', cls.issuer)

    def cms(self, content, identity=None, extra=()):
        cert, key = identity or self.leaf
        return openssl('cms', '-sign', '-binary', '-md', 'sha256', '-signer', cert, '-inkey', key,
                       '-certfile', self.issuer[0], '-outform', 'DER', *extra, data=content)

    def check(self, signature, content, leaf=None):
        apple.verify_cms(signature, content, TEAM, der_hash((leaf or self.leaf)[0]), self.root[0], {der_hash(self.issuer[0])})

    def test_detached_evidence_matches_only_the_pinned_apple_identity_and_content(self):
        evidence = b'{"candidate":"sample"}'
        signed = self.cms(evidence)
        self.check(signed, evidence)
        for signature, content, leaf in ((signed, evidence + b' ', None),
                                         (self.cms(evidence, self.other), evidence, None),
                                         (self.cms(evidence, self.unmarked), evidence, self.unmarked),
                                         (self.cms(evidence, extra=['-nodetach']), evidence, None),
                                         (self.cms(evidence, extra=['-signer', self.other[0], '-inkey', self.other[1]]), evidence, None)):
            with self.subTest(content=content, leaf=leaf), self.assertRaises(ValueError):
                self.check(signature, content, leaf)

    def test_the_leaf_team_and_apple_issuer_each_come_from_separate_trust_configuration(self):
        evidence = b'qualification'
        cms = self.cms(evidence)
        leaf = der_hash(self.leaf[0])
        issuer = der_hash(self.issuer[0])
        for team, root, issuers in [('OTHERS1234', self.root[0], {issuer}),
                                    (TEAM, self.other[0], {issuer}), (TEAM, self.root[0], {leaf})]:
            with self.subTest(team=team, root=root, issuers=issuers), self.assertRaises(ValueError):
                apple.verify_cms(cms, evidence, team, leaf, root, issuers)

    def test_code_directory_covers_the_entire_program_and_its_designated_requirement(self):
        image = b'x' * 5711
        cd = code_directory(image)
        self.check(self.cms(cd), cd)
        apple.check_directory(cd, image, {2: REQUIREMENT}, TEAM)
        for old_cd, binary, req in [(cd, b'X' + image[1:], REQUIREMENT), (cd, image, REQUIREMENT[:-1] + b'X'),
                                     (cd, image + b'X', REQUIREMENT), (cd[:80] + struct.pack('>Q', 0x41) + cd[88:], image, REQUIREMENT),
                                     (cd[:12] + struct.pack('>I', 0x10002) + cd[16:], image, REQUIREMENT)]:
            with self.assertRaises(ValueError):
                apple.check_directory(old_cd, binary, {2: req}, TEAM)

    def make_stage(self, work, damage=''):
        raw, stage = work / 'raw', work / 'stage'
        raw.mkdir()
        stage.mkdir()
        receipt = {'raw_sha256': {}, 'signed_sha256': {}}
        for arch in apple.native.ARCHITECTURES:
            original, signed = work / ('original-' + arch), work / ('signed-' + arch)
            original.mkdir()
            signed.mkdir()
            for name in apple.native.PROGRAMS:
                raw_program = bytearray(fixture(8192, extra_data=name.encode()))
                if arch == 'x86_64':
                    struct.pack_into('<II', raw_program, 4, 0x01000007, 3)
                (original / name).write_bytes(raw_program)
                image = raw_program[:-8192]
                target = arch == 'x86_64' and name == 'tilo-runtime'
                if target and damage == 'payload':
                    image[1040] ^= 1
                cd = code_directory(image)
                cms = self.cms(cd, self.other if target and damage == 'signer' else self.leaf)
                if target and damage == 'adhoc':
                    cms = b''
                parts = ((0, cd), (2, REQUIREMENT), (0x10000, struct.pack('>II', 0xfade0b01, 8 + len(cms)) + cms))
                pos, index = 12 + len(parts) * 8, bytearray()
                for kind, blob in parts:
                    index.extend(struct.pack('>II', kind, pos))
                    pos += len(blob)
                seal = struct.pack('>III', 0xfade0cc0, pos, len(parts)) + index + b''.join(b for _, b in parts)
                self.assertLessEqual(len(seal), 8192)
                (signed / name).write_bytes(image + seal + bytes(8192 - len(seal)))
            filename = f'tilo-macos-{arch}.tar.gz'
            for directory, source, field in ((raw, original, 'raw_sha256'), (stage, signed, 'signed_sha256')):
                apple.native.pack(source, directory / filename)
                receipt[field][filename] = apple.native.digest(directory / filename)
        evidence = json.dumps(receipt).encode()
        (stage / 'qualification.json').write_bytes(evidence)
        (stage / 'qualification.cms').write_bytes(self.cms(evidence))
        return raw, stage

    def test_the_approved_receipt_cannot_bless_an_adhoc_foreign_or_changed_program(self):
        for damage, error in (('', None), ('adhoc', 'empty'), ('signer', 'signer'), ('payload', 'file bytes changed')):
            with self.subTest(damage=damage), tempfile.TemporaryDirectory() as tmp:
                raw, stage = self.make_stage(Path(tmp), damage)
                trust = (TEAM, der_hash(self.leaf[0]), self.root[0], {der_hash(self.issuer[0])})
                if error:
                    with self.assertRaisesRegex(ValueError, error):
                        apple.verify_stage(raw, stage, *trust)
                else:
                    apple.verify_stage(raw, stage, *trust)
                    # A staged archive replaced after the receipt was signed is rejected at unpack.
                    (stage / 'tilo-macos-aarch64.tar.gz').write_bytes((stage / 'tilo-macos-x86_64.tar.gz').read_bytes())
                    with self.assertRaisesRegex(ValueError, 'SHA-256'):
                        apple.verify_stage(raw, stage, *trust)

    def test_apple_compiles_the_same_common_designated_requirement(self):
        if not Path('/usr/bin/csreq').exists():
            self.skipTest('csreq runs on macOS')
        dest = self.work / 'csreq.bin'
        subprocess.run(['/usr/bin/csreq', '-r=designated => ' + apple.native.product_requirement(TEAM), '-b', dest], check=True)
        self.assertEqual(dest.read_bytes(), REQUIREMENT)

if __name__ == '__main__':
    unittest.main()
