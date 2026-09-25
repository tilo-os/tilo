"""Exercise effective Podman argv and controller epoch admission without builds."""
import os
from pathlib import Path
import subprocess
import textwrap
import tempfile
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

    def test_workflow_anchor_validation_and_optional_argv(self):
        workflow = (Path(__file__).resolve().parents[2]/'.forgejo/workflows/development-compile.yml').read_text()
        self.assertEqual(workflow.count('ASSEMBLY_LAYER_EPOCH_ANCHOR: ${{ inputs.assembly_layer_epoch_anchor }}'), 3)
        self.assertIn('"${layer_epoch_args[@]}"', workflow)
        self.assertIn("fetch-depth: ${{ inputs.assembly_layer_epoch_anchor != '' && '0' || '5' }}", workflow)
        self.assertIn("filter: ${{ inputs.assembly_layer_epoch_anchor != '' && 'blob:none' || '' }}", workflow)
        self.assertLess(workflow.index('Validate layer timestamp anchor before building tools'), workflow.index('python3 os/dev/build-compile-tools.py'))
        guard = textwrap.dedent('          anchor = ' + workflow.split('          anchor = ', 1)[1].split("          s = importlib.util.spec_from_file_location('compile'", 1)[0])
        valid = {'ASSEMBLY_LAYER_EPOCH_ANCHOR':'b'*40, 'COMPILE_MODE':'full-build',
                 'RECONSTRUCTED_NATIVE_RECEIPT':'{}', 'WPE_ADOPTION_RECEIPT':'{}', 'NATIVE_SOURCE_COMPATIBILITY':'{}'}
        import re
        exec(guard, {'e':valid, 're':re})
        for field,value in [('COMPILE_MODE','cache-only'), ('COMPILE_MODE','native-payload'),
                            ('RECONSTRUCTED_NATIVE_RECEIPT',''), ('WPE_ADOPTION_RECEIPT',''),
                            ('ASSEMBLY_LAYER_EPOCH_ANCHOR','main')]:
            with self.subTest(field=field,value=value), self.assertRaises(ValueError):
                exec(guard, {'e':dict(valid, **{field:value}), 're':re})
        exec(guard, {'e':dict(valid, ASSEMBLY_LAYER_EPOCH_ANCHOR='', COMPILE_MODE='cache-only'), 're':re})
        shell = textwrap.dedent('          layer_epoch_args=()' + workflow.split('          layer_epoch_args=()',1)[1].split('          if [ -n "$NATIVE_SOURCE_COMPATIBILITY" ]',1)[0])
        for anchor,expected in [('', b''), ('b'*40, b'--assembly-layer-epoch-anchor\0'+b'b'*40+b'\0')]:
            result = subprocess.run(['bash','-c',shell+'\nfor arg in "${layer_epoch_args[@]}"; do printf "%s\\0" "$arg"; done'],
                env=dict(os.environ, ASSEMBLY_LAYER_EPOCH_ANCHOR=anchor), capture_output=True)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(result.stdout,expected)

    def test_workflow_ancestry_gate_executes_against_source_checkout(self):
        workflow = (Path(__file__).resolve().parents[2]/'.forgejo/workflows/development-compile.yml').read_text()
        step = workflow.split('- name: Validate layer timestamp anchor before building tools',1)[1]
        shell = textwrap.dedent(step.split('        run: |\n',1)[1].split('      # The workflow checkout',1)[0])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repo = root/'application-source';repo.mkdir()
            controller=root/'controller';controller.mkdir()
            def git(*args):
                return subprocess.check_output(['git','-C',str(repo),*args],stderr=subprocess.DEVNULL,text=True).strip()
            git('init');git('config','user.email','fixture@example.invalid');git('config','user.name','Fixture')
            git('commit','--allow-empty','-m','anchor');anchor=git('rev-parse','HEAD')
            git('commit','--allow-empty','-m','source');source=git('rev-parse','HEAD')
            env=dict(os.environ,SOURCE_REVISION=source,ASSEMBLY_LAYER_EPOCH_ANCHOR=anchor)
            result=subprocess.run(['bash','-c',shell],cwd=controller,env=env,capture_output=True)
            self.assertEqual(result.returncode,0,result.stderr)
            env['ASSEMBLY_LAYER_EPOCH_ANCHOR']='f'*40
            self.assertNotEqual(subprocess.run(['bash','-c',shell],cwd=controller,env=env,capture_output=True).returncode,0)
            env.update(ASSEMBLY_LAYER_EPOCH_ANCHOR=anchor,SOURCE_REVISION=anchor)
            self.assertNotEqual(subprocess.run(['bash','-c',shell],cwd=controller,env=env,capture_output=True).returncode,0)

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
