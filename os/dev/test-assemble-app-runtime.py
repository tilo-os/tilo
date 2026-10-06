#!/usr/bin/env python3
"""Exercise actual private cohort assembly namespaces without compiling apps."""
import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SOURCE = Path(__file__).with_name('assemble-app-runtime.py')
SPEC = importlib.util.spec_from_file_location('assemble_runtime', SOURCE)
ASSEMBLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ASSEMBLER)


class AssemblyNamespaces(unittest.TestCase):
    def test_generated_pin_stage_becomes_readonly_for_the_normal_installer(self):
        with tempfile.TemporaryDirectory(prefix='assembly-test-') as directory:
            stage = Path(directory)
            argv = ASSEMBLER.namespace_command(SOURCE.parents[2], stage,
                                                stage, 'thin-app')
            inside = argv.index('python3')
            generated = subprocess.run(argv[:inside] + ['python3', '-c',
                "from pathlib import Path; Path('/usr/share/tilo/runtime-bundle-publisher.hex').write_text('11'*32)"],
                capture_output=True, text=True)
            self.assertEqual(generated.returncode, 0, generated.stderr)
            argv = ASSEMBLER.namespace_command(SOURCE.parents[2], stage,
                                               stage, 'thin-app', packaging=True)
            inside = argv.index('python3')
            script = (
                "import os,pathlib; p=pathlib.Path('/usr/share/tilo/runtime-bundle-publisher.hex'); "
                "assert p.read_text()=='11'*32; assert p.stat().st_uid==0; "
                "assert p.stat().st_mode & 0o022 == 0; "
                "assert os.statvfs(p).f_flag & os.ST_RDONLY; "
                "assert pathlib.Path('/usr/share/tilo').stat().st_uid==0; "
                "a=pathlib.Path('/usr/share/tilo/app-publisher.hex'); "
                "a.write_text('22'*32); assert a.read_text()=='22'*32"
            )
            result = subprocess.run(argv[:inside] + ['python3', '-c', script],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_permissive_umask_preserves_immutable_public_pin_modes(self):
        previous = os.umask(0o002)
        try:
            self.test_generated_pin_stage_becomes_readonly_for_the_normal_installer()
        finally:
            os.umask(previous)

    def test_catalogue_uses_all_native_manifests_and_one_release(self):
        rows = ASSEMBLER.applications(SOURCE.parents[2])
        self.assertEqual(len(rows), len(list((SOURCE.parents[2]/'apps').glob('*/tilo-app.toml'))))
        self.assertEqual(len({row[2] for row in rows}), 1)


if __name__ == '__main__':
    unittest.main()
