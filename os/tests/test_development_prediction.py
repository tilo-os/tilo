"""Offline prediction boundary tests; mocked tools are not native PCR proof."""
import base64
import hashlib
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
import development_prediction as prediction
from test_development_boot import synthetic_uki, SEAL


def document(**changes):
    return json.dumps({'generation': 7, 'pcr_bank': 'sha256', 'pcrs': [11],
                       'approved_policy': base64.b64encode(b'p' * 32).decode(),
                       'signature': base64.b64encode(b's' * 256).decode(), **changes}).encode()


class PredictionTests(unittest.TestCase):
    def test_prediction_binding(self):
        with self.subTest(phase="prediction_binds_exact_generation_bank_and_selection"):
            self.assertEqual(prediction.prediction_document(document(), 7), (b'p' * 32, b's' * 256))
            for change in ({'generation': True}, {'generation': 8}, {'pcr_bank': 'sha384'},
                           {'pcrs': [7, 11]}, {'approved_policy': 'cA=='}, {'signature': '!'},
                           {'signature': base64.b64encode(b's' * 128).decode()}):
                with self.subTest(change=change), self.assertRaises(ValueError):
                    prediction.prediction_document(document(**change), 7)
            with self.assertRaises(ValueError):
                prediction.prediction_document(document()[:-1] + b',"generation":7}', 7)
        with self.subTest(phase="native_section_geometry_preserves_explicit_dtb"):
            for architecture in ('x86_64', 'aarch64'):
                data = bytearray(synthetic_uki(architecture))
                # Fourth section occupies the final available section-table slot.
                struct.pack_into('<H', data, 70, 4)
                struct.pack_into('<I', data, 88 + 56, 20480)
                data.extend(b'\0' * 512)
                offset = 328 + 3 * 40
                data[offset:offset + 8] = b'.dtb\0\0\0\0'
                struct.pack_into('<IIII', data, offset + 8, 3, 16384, 512, 2048)
                data[2048:2051] = b'dtb'
                self.assertEqual(prediction.measurement_sections(data, architecture)['dtb'], b'dtb')
                for name in (b'.dtbauto', b'.profile', b'.unknown'):
                    changed = bytearray(data)
                    changed[offset:offset + 8] = name
                    with self.assertRaises(ValueError):
                        prediction.measurement_sections(changed, architecture)
                struct.pack_into('<I', data, offset + 8, 513)
                with self.assertRaises(ValueError):
                    prediction.measurement_sections(data, architecture)
        with self.subTest(phase="orchestration_binds_snapshots_signatures_and_recomputed_policy"):
            for failure in (None, 'target', 'key', 'policy', 'missing_target', 'policy_signature', 'uki_signature'):
                with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    data = bytearray(synthetic_uki())
                    struct.pack_into('<H', data, 70, 4)
                    data[448:456] = b'.sdmagic'
                    struct.pack_into('<IIII', data, 456, 0, 16384, 512, 2048)
                    struct.pack_into('<I', data, 88 + 56, 20480)
                    data.extend(b'#### LoaderInfo: systemd-stub 259.8-1.fc44 ####'.ljust(512, b'\0'))
                    data = bytes(data)
                    target_hash = hashlib.sha256(data).hexdigest()
                    key_hash = hashlib.sha256(b'public DER').hexdigest()
                    calls = []

                    def snapshot(source, destination, limit):
                        if source.name == 'tilo-target.efi':
                            value = data
                        elif source.suffix == '.json':
                            value = document()
                        elif source.suffix == '.sig':
                            value = b'd' * 256
                        else:
                            value = b'public PEM'
                        destination.write_bytes(value)
                        return value

                    def command(argv):
                        calls.append(argv)
                        if argv[1] == 'rsa-public-key-info':
                            return f'2048 {key_hash}\n'.encode()
                        if argv[1] == 'verify-rsa-sha256':
                            message = Path(argv[3]).read_bytes()
                            if argv[3].endswith('policy-message'):
                                if failure == 'policy_signature':
                                    raise ValueError('invalid policy signature')
                                self.assertEqual(message, b'p' * 32 + struct.pack('>Q', 7))
                            else:
                                if failure == 'uki_signature':
                                    raise ValueError('invalid detached UKI signature')
                                self.assertEqual(message, data)
                            return b''
                        if argv[1] == '--version':
                            return b'systemd 259 (259.8-1.fc44)\n'
                        self.assertEqual(argv[0], prediction.MEASURE)
                        self.assertIn('--phase=enter-initrd', argv)
                        self.assertFalse(any('--current' in arg or '--tpm2-device' in arg for arg in argv))
                        return json.dumps({'sha256': [{'pcrs': [11], 'pol': ('71' if failure == 'policy' else '70') * 32}]}).encode()

                    ukis = [] if failure == 'missing_target' else [{'path': prediction.TARGET.as_posix(),
                        'sha256': '0' * 64 if failure == 'target' else target_hash}]
                    with patch.object(prediction.boot, 'validate_mounted_boot', return_value={'ukis': ukis}), \
                         patch.object(prediction.boot.trust, 'validate', return_value={'identities': {
                             'tpm2-pcr-signing.public.pem': '0' * 64 if failure == 'key' else key_hash}}), \
                         patch.object(prediction.boot.hardware, 'trusted_path', side_effect=lambda root, path, **kw: root / path), \
                         patch.object(prediction.boot.loader, 'snapshot', side_effect=snapshot), \
                         patch.object(prediction.boot.loader, 'command', side_effect=command):
                        args = (root, root, 'a' * 64, 'x86_64', 7, SEAL, '259.8-1.fc44')
                        if failure:
                            with self.assertRaises(ValueError):
                                prediction.verify_prediction(*args)
                        else:
                            receipt = prediction.verify_prediction(*args)
                            self.assertEqual(receipt['target_uki_sha256'], target_hash)
                            self.assertFalse(receipt['installable'])
                            self.assertFalse(receipt['proof']['live_pcr'])
                            self.assertEqual(sum(argv[1] == 'verify-rsa-sha256' for argv in calls), 2)

    def test_prediction_rejects_wrong_host_inputs(self):
        with self.subTest(phase="rejects_ambiguous_or_wrong_host_measurements"):
            valid = {'pcrs': [11], 'pol': '70' * 32}
            self.assertEqual(prediction.measured_policy(json.dumps({'sha256': [valid]})), b'p' * 32)
            for value in ({'sha256': []}, {'sha256': [valid, valid]}, {'sha256': [valid], 'sha1': []},
                          {'sha256': [valid | {'pcrs': [7, 11]}]}, {'sha256': [valid | {'pol': 'aa'}]}):
                with self.assertRaises(ValueError):
                    prediction.measured_policy(json.dumps(value))
        with self.subTest(phase="exact_pinned_host_and_signed_stub_version_required"):
            marker = b'#### LoaderInfo: systemd-stub 259.8-1.fc44 ####'
            host = b'systemd 259 (259.8-1.fc44)\n+TPM2\n'
            prediction.check_systemd_version(marker, '259.8-1.fc44', host)
            for data, version, output in ((b'', '259.8-1.fc44', host),
                                          (marker * 2, '259.8-1.fc44', host),
                                          (marker, '259.9-1.fc44', host),
                                          (marker, '259.8-1.fc44', b'systemd 258 (258)')):
                with self.assertRaises(ValueError):
                    prediction.check_systemd_version(data, version, output)
            # A marker in arbitrary UKI payload cannot substitute for .sdmagic.
            with self.assertRaises(ValueError):
                prediction.stub_metadata(synthetic_uki() + marker)


if __name__ == '__main__':
    unittest.main()
