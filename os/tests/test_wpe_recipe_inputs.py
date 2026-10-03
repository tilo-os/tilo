#!/usr/bin/env python3
"""Small fixture: WPE dependency keys ignore unrelated OS Containerfile edits."""
import importlib.util
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    'wpe_artifact', Path(__file__).resolve().parents[1] / 'dev/wpe-stage-artifact.py')
wpe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wpe)


class WpeRecipeInputIsolation(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name)
        (self.repo / 'os/dev').mkdir(parents=True)
        self.assets = self.repo / 'assets'
        self.assets.mkdir()
        script = []
        for variable in ('LIBWPE', 'FDO', 'WPEWK'):
            name = variable.lower() + '-1.tar.xz'
            data = variable.encode()
            (self.assets / name).write_bytes(data)
            script.extend((f'{variable}={name}', f'{variable}_SHA256={wpe.sha(data)}'))
        (self.repo / 'os/dev/build-wpe.sh').write_text('\n'.join(script) + '\n')
        (self.repo / 'os/Containerfile.wpe').write_text(
            'FROM ${WPE_BUILDROOT_REF} AS wpe-builder-1\nCOPY os/dev/build-wpe.sh /build-wpe.sh\n')
        (self.repo / 'os/Containerfile').write_text(
            'FROM scratch AS wpe-builder-1\nRUN ["/must-not-compile-here"]\n'
            'FROM scratch AS rootfs\n# initramfs sealing windows comment\n')
        self.buildroot = 'registry.tilo.so/tilo/tilo-wpe-buildroot@sha256:' + '1' * 64

    def key(self):
        inputs = wpe.dependency_inputs(self.repo, self.assets, 'x86_64', '0', self.buildroot)
        self.assertEqual(set(inputs['recipes']), {'os/Containerfile.wpe', 'os/dev/build-wpe.sh'})
        return inputs['recipes'], wpe.sha(wpe.canonical(inputs))

    def test_unrelated_os_edit_preserves_key_and_wpe_edit_invalidates(self):
        recipes, dependency_key = self.key()
        (self.repo / 'os/Containerfile').write_text(
            (self.repo / 'os/Containerfile').read_text() + '# unrelated initramfs edit\n')
        recipes_after, key_after = self.key()
        self.assertEqual(recipes_after, recipes)
        self.assertEqual(key_after, dependency_key)

        (self.repo / 'os/Containerfile.wpe').write_text(
            (self.repo / 'os/Containerfile.wpe').read_text() + '# real WPE input\n')
        _, changed = self.key()
        self.assertNotEqual(changed, dependency_key)

        (self.repo / 'os/dev/build-wpe.sh').write_text(
            (self.repo / 'os/dev/build-wpe.sh').read_text() + '# script input\n')
        recipes_script, key_script = self.key()
        self.assertNotEqual(key_script, changed)
        self.assertNotEqual(recipes_script['os/dev/build-wpe.sh'], recipes['os/dev/build-wpe.sh'])


if __name__ == '__main__':
    unittest.main()
