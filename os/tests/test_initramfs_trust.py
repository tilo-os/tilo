import gzip
import importlib.util
from pathlib import Path
import stat
import lzma
import os
import signal
import subprocess
import sys
from unittest.mock import patch
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'tools/tests'))
from hang import HANG_BOUND  # noqa: E402

SPEC = importlib.util.spec_from_file_location('initramfs', Path(__file__).resolve().parents[1] / 'dev/initramfs-trust.py')
verifier = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verifier)
KEY = b'-----BEGIN PUBLIC KEY-----\nfixture public bytes\n-----END PUBLIC KEY-----\n'


def cpio(entries):
    result = bytearray()
    for index, (name, content) in enumerate([*entries, ('TRAILER!!!', b'')]):
        encoded = name.encode() + b'\0'
        fields = [index, stat.S_IFREG | 0o644, 0, 0, 1, 0, len(content), 0, 0, 0, 0, len(encoded), 0]
        header = b'070701' + ''.join(f'{field:08x}' for field in fields).encode() + encoded
        result.extend(header + bytes((-len(header)) % 4))
        result.extend(content + bytes((-len(content)) % 4))
    return bytes(result) + bytes((-len(result)) % 512)


class InitramfsTests(unittest.TestCase):
    def verify(self, data):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'initramfs'
            path.write_bytes(data)
            return verifier.verify(path, KEY, scratch_parent=Path(temporary))

    def test_archive_parsing(self):
        with self.subTest(phase="plain_and_early_plus_gzip_exact_bytes"):
            main = cpio([(verifier.KEY, KEY)])
            for data, count in [(main, 1), (gzip.compress(main), 1), (lzma.compress(main), 1),
                                (cpio([('early_cpio', b'1')]) + gzip.compress(main), 2)]:
                result = self.verify(data)
                self.assertEqual(result['cpio_archives'], count)
                self.assertEqual(result['matching_occurrences'], 1)
        with self.subTest(phase="missing_stale_duplicate_corrupt_and_truncated_rejected"):
            good = cpio([(verifier.KEY, KEY)])
            compressed = bytearray(gzip.compress(good))
            compressed[-5] ^= 1
            cases = [cpio([('other', KEY)]), cpio([(verifier.KEY, b'old key')]),
                     cpio([(verifier.KEY, KEY), (verifier.KEY, KEY)]),
                     cpio([(verifier.KEY, KEY)]) + gzip.compress(good),
                     bytes(compressed), gzip.compress(good)[:-4], good[:100],
                     gzip.compress(good) + b'garbage', b'unsupported format']
            for index, data in enumerate(cases):
                with self.subTest(index=index), self.assertRaises(verifier.VerificationError):
                    self.verify(data)
        with self.subTest(phase="invalid_numeric_header_and_redirected_key_are_rejected"):
            original = cpio([(verifier.KEY, KEY)])
            bad_numeric = bytearray(original)
            bad_numeric[54:62] = b'-0000001'
            symlink = bytearray(original)
            symlink[14:22] = f'{stat.S_IFLNK | 0o777:08x}'.encode()
            hardlink = bytearray(original)
            hardlink[38:46] = b'00000002'
            for data in (bad_numeric, symlink, hardlink):
                with self.assertRaises(verifier.VerificationError):
                    self.verify(bytes(data))

    def test_deadline_kills_descendants_after_tool_leader_exits(self):
        with tempfile.TemporaryDirectory() as temporary:
            pidfile = Path(temporary) / 'descendant-pid'
            tool = (
                'import os,subprocess,sys; from pathlib import Path; '
                'child=subprocess.Popen([sys.executable,"-c","import signal; signal.pause()"]); '
                f'Path({str(pidfile)!r} + ".pending").write_text(str(child.pid)); '
                f'os.replace({str(pidfile)!r} + ".pending", {str(pidfile)!r})'
            )
            # The deadline timer fires once the tool's descendant exists, never
            # while a loaded runner is still starting the tool.
            script = f'''
import importlib.util,sys,threading,time,types
from pathlib import Path
spec=importlib.util.spec_from_file_location('verifier', {str(SPEC.origin)!r})
verifier=importlib.util.module_from_spec(spec);spec.loader.exec_module(verifier)
class AtDescendant(threading.Thread):
    def __init__(self, _seconds, function):
        super().__init__(daemon=True)
        self.function, self.cancelled = function, threading.Event()
    def run(self):
        deadline = time.monotonic() + {HANG_BOUND!r}
        while not Path({str(pidfile)!r}).exists() and time.monotonic() < deadline:
            if self.cancelled.wait(0.01):
                return
        self.function()
    def cancel(self):
        self.cancelled.set()
verifier.threading = types.SimpleNamespace(Timer=AtDescendant, Event=threading.Event)
try:
    with verifier.child([sys.executable, '-c', {tool!r}], scratch=Path({temporary!r}),
                        deadline=time.monotonic()+{HANG_BOUND!r}) as output:
        output.read(1)
except verifier.VerificationError:
    sys.exit(0)
sys.exit(1)
'''
            try:
                result = subprocess.run([sys.executable, '-c', script], capture_output=True,
                                        text=True, timeout=HANG_BOUND)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(pidfile.exists(), 'tool must actually launch its descendant')
            finally:
                if pidfile.exists():
                    try:
                        os.kill(int(pidfile.read_text()), signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    def test_resource_and_format_limits(self):
        with self.subTest(phase="expansion_budget_and_child_deadline_reject"):
            main = gzip.compress(cpio([(verifier.KEY, KEY)]))
            with patch.object(verifier, 'MAX_EXPANDED', 32):
                # Default argument is fixed at definition; an oversized entry still
                # fails against the patched explicit per-entry bound.
                with self.assertRaises(verifier.VerificationError):
                    self.verify(main)
            with patch.object(verifier, 'TIMEOUT', 0):
                with self.assertRaises(verifier.VerificationError):
                    self.verify(main)
        with self.subTest(phase="unsupported_erofs_and_path_traversal_fail"):
            for name in ('erofs-root.img', '../' + verifier.KEY, '/' + verifier.KEY, 'usr//share/tilo/key'):
                with self.subTest(name=name), self.assertRaises(verifier.VerificationError):
                    self.verify(cpio([(name, KEY)]))

if __name__ == '__main__':
    unittest.main()
