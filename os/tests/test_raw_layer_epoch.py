"""Exercise effective Podman argv and controller epoch admission without builds."""
import os
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch
from test_assembly_registry_cache import AssemblyCache, d

class RawLayerEpoch(unittest.TestCase):
    def env(self):
        env = AssemblyCache().env()
        env['SOURCE_DATE_EPOCH'] = '200'
        return env

    def test_requested_anchor_without_adopted_wpe_fails_before_source_selection(self):
        args = ['--mode', 'full-build', '--revision', 'c'*40, '--branch', 'main',
                '--reconstructed-native-receipt', '/tmp/native.json',
                '--assembly-layer-epoch-anchor', 'b'*40]
        with patch.object(d, 'select_source') as select:
            with self.assertRaisesRegex(ValueError, 'adopted-WPE'):
                d.main(args)
            select.assert_not_called()

    def test_anchor_records_both_epochs_without_changing_source(self):
        env = self.env()
        with patch.object(d, 'run', side_effect=['', '200', '100']) as run:
            receipt = d.assembly_cache(env, 'aarch64', 'full-build', env['TILO_NATIVE_BUILDER_IMAGE_ID'], 'a'*64, 'b'*40, 'c'*40)
        self.assertEqual(run.call_args_list[0].args, ('git', 'merge-base', '--is-ancestor', 'b'*40, 'c'*40))
        self.assertEqual(env['SOURCE_DATE_EPOCH'], '200')
        self.assertEqual(env['TILO_RAW_LAYER_EPOCH'], '100')
        self.assertEqual(receipt['layer_normalization']['source_date_epoch'], 200)
        self.assertEqual(receipt['layer_normalization']['anchor_revision'], 'b'*40)

    def test_bad_anchor_or_epoch_fails_without_setting_override(self):
        for anchor, answers in [('main', []), ('b'*40, ['', '201', '100']), ('b'*40, ['', '200', '201'])]:
            env = self.env()
            with self.subTest(anchor=anchor, answers=answers), patch.object(d, 'run', side_effect=answers), self.assertRaises(ValueError):
                d.assembly_cache(env, 'aarch64', 'full-build', env['TILO_NATIVE_BUILDER_IMAGE_ID'], 'a'*64, anchor, 'c'*40)
            self.assertNotIn('TILO_RAW_LAYER_EPOCH', env)
        env = self.env()
        with patch.object(d, 'run', side_effect=subprocess.CalledProcessError(1, 'git')), self.assertRaises(subprocess.CalledProcessError):
            d.assembly_cache(env, 'aarch64', 'full-build', env['TILO_NATIVE_BUILDER_IMAGE_ID'], 'a'*64, 'b'*40, 'c'*40)

    def invoke(self, overrides=None, target='rootfs'):
        source = (Path(__file__).resolve().parents[1]/'dev/build-image.sh').read_text()
        function = source.split('build_image() {', 1)[1].split('\n}\n', 1)[0]
        env = dict(os.environ, SOURCE_DATE_EPOCH='200', TILO_RAW_ROOTFS_ONLY='1', TILO_PUBLIC_TRUST_MODE='development', TILO_NATIVE_BUILDER_IMAGE_ID='sha256:'+'a'*64, TILO_WPE_ADOPTION_SHA256='b'*64)
        env.pop('TILO_RAW_LAYER_EPOCH', None)
        env.update(overrides or {})
        script = 'die() { echo "$*" >&2; exit 19; }; podman() { printf "%s\\0" "$@"; }; build_image() {'+function+'\n}\nbuild_image seal "" --target '+target+' --iidfile /tmp/raw.iid\n'
        return subprocess.run(['bash', '-c', script], env=env, capture_output=True)

    def test_effective_cli_epoch_changes_but_source_argument_does_not(self):
        for override, expected in [({}, '200'), ({'TILO_RAW_LAYER_EPOCH':'100'}, '100')]:
            result = self.invoke(override)
            self.assertEqual(result.returncode, 0, result.stderr)
            args = result.stdout.decode().split('\0')
            self.assertEqual(args[args.index('--source-date-epoch')+1], expected)
            self.assertIn('SOURCE_DATE_EPOCH=200', args)

    def test_guard_rejects_other_modes_and_invalid_values(self):
        for key,value in [('TILO_RAW_ROOTFS_ONLY','0'), ('TILO_PUBLIC_TRUST_MODE','production'), ('TILO_NATIVE_BUILDER_IMAGE_ID','mutable'), ('TILO_WPE_ADOPTION_SHA256',''), ('TILO_RAW_LAYER_EPOCH','-1'), ('TILO_RAW_LAYER_EPOCH','253402300800')]:
            values={'TILO_RAW_LAYER_EPOCH':'100',key:value}
            with self.subTest(key=key,value=value):
                self.assertEqual(self.invoke(values).returncode,19)
        self.assertEqual(self.invoke({'TILO_RAW_LAYER_EPOCH':'100'}, target='final').returncode,19)

if __name__ == '__main__':
    unittest.main()
