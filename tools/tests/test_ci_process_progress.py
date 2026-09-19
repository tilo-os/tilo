import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('progress', Path(__file__).parents[1] / 'ci_process_progress.py')
progress = importlib.util.module_from_spec(spec)
spec.loader.exec_module(progress)


class ProcessProgressTests(unittest.TestCase):
    def put(self, base, pid, ppid, ticks=3, io=True):
        path = base / str(pid)
        path.mkdir()
        fields = ['0'] * 22
        fields[0], fields[1] = 'S', str(ppid)
        fields[11], fields[12], fields[21] = str(ticks), '2', '42'
        (path / 'stat').write_text(f'{pid} (name with ) spaces) ' + ' '.join(fields))
        (path / 'cmdline').write_text('SECRET_TOKEN=never-output')
        if io:
            (path / 'io').write_text('read_bytes: 123\nwrite_bytes: 456\n')

    def test_descendants_numeric_only_and_missing_io_unknown(self):
        with tempfile.TemporaryDirectory() as tmp:
            proc = Path(tmp)
            self.put(proc, 100, 1)
            self.put(proc, 101, 100)
            self.put(proc, 102, 101, 8, io=False)
            self.put(proc, 900, 1)
            result = progress.snapshot(100, proc)
            self.assertEqual([p['pid'] for p in result['processes']], [102, 101])
            self.assertEqual(result['processes'][0]['cpu_ticks'], 10)
            self.assertNotIn('read_bytes', result['processes'][0])
            self.assertEqual(result['processes'][1]['read_bytes'], 123)
            self.assertNotIn('SECRET', str(result))
            self.assertNotIn('name with', str(result))

    def test_bounded_output_and_disappearing_process(self):
        with tempfile.TemporaryDirectory() as tmp:
            proc = Path(tmp)
            for pid in range(101, 121):
                self.put(proc, pid, 100)
            (proc / '999').mkdir()
            result = progress.snapshot(100, proc)
            self.assertEqual(result['descendant_count'], 20)
            self.assertEqual(len(result['processes']), 16)
            self.assertTrue(result['truncated'])
