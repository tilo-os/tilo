import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'progress', Path(__file__).parents[1] / 'ci/process_progress.py'
)
progress = importlib.util.module_from_spec(spec)
spec.loader.exec_module(progress)


class ProcessProgressTests(unittest.TestCase):
    def setUp(self):
        # The production helper is Linux-only; synthetic procfs fixtures also
        # run on Windows hosts that do not expose os.sysconf.
        mock = patch.object(progress.os, 'sysconf', create=True,
                            side_effect=lambda name: {'SC_CLK_TCK': 100, 'SC_PAGE_SIZE': 4096}[name])
        mock.start()
        self.addCleanup(mock.stop)

    def put(self, base, pid, ppid, ticks=3, io=True):
        path = base / str(pid)
        path.mkdir()
        fields = ['0'] * 22
        fields[0], fields[1] = 'S', str(ppid)
        fields[11], fields[12], fields[19], fields[21] = str(ticks), '2', '12345', '42'
        (path / 'stat').write_text(f'{pid} (name with ) spaces) ' + ' '.join(fields))
        (path / 'cmdline').write_text('SECRET_TOKEN=never-output')
        if io:
            (path / 'io').write_text('read_bytes: 123\nwrite_bytes: 456\n')

    def test_process_progress_reports_only_numeric_bounded_or_unknown_values(self):
        with self.subTest(phase='descendants numeric only and missing io unknown'):
            with tempfile.TemporaryDirectory() as tmp:
                proc = Path(tmp)
                self.put(proc, 100, 1)
                self.put(proc, 101, 100)
                self.put(proc, 102, 101, 8, io=False)
                self.put(proc, 900, 1)
                result = progress.snapshot(100, proc)
                self.assertEqual([p['pid'] for p in result['processes']], [102, 101])
                self.assertEqual(result['processes'][0]['cpu_ticks'], 10)
                self.assertEqual(result['processes'][0]['start_ticks'], 12345)
                self.assertEqual(result['clock_ticks_per_second'], 100)
                self.assertEqual(result['page_size_bytes'], 4096)
                self.assertNotIn('read_bytes', result['processes'][0])
                self.assertEqual(result['processes'][1]['read_bytes'], 123)
                self.assertNotIn('SECRET', str(result))
                self.assertNotIn('name with', str(result))
        with self.subTest(phase='bounded output and disappearing process'):
            with tempfile.TemporaryDirectory() as tmp:
                proc = Path(tmp)
                self.put(proc, 100, 1)
                for pid in range(101, 121):
                    self.put(proc, pid, 100)
                (proc / '999').mkdir()
                result = progress.snapshot(100, proc)
                self.assertEqual(result['descendant_count'], 20)
                self.assertEqual(len(result['processes']), 16)
                self.assertTrue(result['truncated'])
        with self.subTest(phase='missing or unreadable root is unknown not empty'):
            with tempfile.TemporaryDirectory() as tmp:
                proc = Path(tmp)
                with self.assertRaisesRegex(RuntimeError, 'unavailable'):
                    progress.snapshot(100, proc)
                root = proc / '100'
                root.mkdir()
                (root / 'stat').write_text('incomplete')
                with self.assertRaisesRegex(RuntimeError, 'unavailable'):
                    progress.snapshot(100, proc)
