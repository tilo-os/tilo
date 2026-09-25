import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'dev'))
SPEC = importlib.util.spec_from_file_location('consumer_compile', Path(__file__).resolve().parents[1] / 'dev/compile-development-os.py')
d = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(d)
import native_resume


class ConsumerTests(unittest.TestCase):
    def inputs(self):
        return dict(wpe='code.podesta.ai/qos/tilo-wpe-buildroot@sha256:'+'a'*64,
            generation='7', revision=d.RECONSTRUCTION_SOURCE, branch='refs/heads/main', mode='full-build',
            architecture='aarch64', kernel_candidate='code.podesta.ai/qos/kernel@sha256:'+'b'*64,
            fex_payload_candidate='code.podesta.ai/qos/fex@sha256:'+'c'*64,
            windows_guest_candidate='code.podesta.ai/qos/guest@sha256:'+'d'*64,
            reconstructed_native_receipt='reconstructed.json', reconstructed_native_sha256='e'*64,
            approved_reconstruction_sha256='f'*64)

    def test_opt_in_needs_two_independent_approvals_and_frozen_source(self):
        d.validate_inputs(**self.inputs())
        for change in ({'approved_reconstruction_sha256':''}, {'reconstructed_native_sha256':''},
                       {'reconstructed_native_receipt':None}, {'revision':'0'*40}, {'mode':'native-payload'},
                       {'native_builder_receipt':'legacy.json','native_builder_sha256':'1'*64}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                d.validate_inputs(**(self.inputs() | change))

    def test_resume_is_distinct_from_import_and_limited_to_native_payload(self):
        value = self.inputs() | dict(mode='native-payload', reconstructed_native_receipt=None,
            reconstructed_native_sha256='', approved_reconstruction_sha256='', resume_checkpoint=[['partial.json','a'*64]])
        d.validate_inputs(**value)
        for change in ({'mode':'full-build'}, {'resume_checkpoint':[['partial.json','bad']]},
                       {'resume_checkpoint':[['partial.json','a'*64]]*5}):
            with self.assertRaises(ValueError): d.validate_inputs(**(value|change))

    def test_reconstructed_import_failure_never_uses_legacy_importer(self):
        args = SimpleNamespace(**self.inputs(), native_builder_receipt=None, native_builder_sha256='')
        native = Mock()
        with patch.object(native_resume, 'import_reconstructed', side_effect=ValueError('inventory differs')) as importer:
            with self.assertRaisesRegex(ValueError, 'inventory differs'):
                d.import_selected_native(native, args, {'source':'frozen'})
            importer.assert_called_once_with(native, 'reconstructed.json', 'e'*64, {'source':'frozen'}, 'f'*64)
        native.import_artifact.assert_not_called()
        args.reconstructed_native_receipt = None
        args.native_builder_receipt, args.native_builder_sha256 = 'legacy.json', 'a'*64
        d.import_selected_native(native, args, {})
        native.import_artifact.assert_called_once_with('legacy.json','a'*64,{})

    def test_source_selection_checks_both_revisions_and_preserves_recipe(self):
        with tempfile.TemporaryDirectory() as temp:
            source=Path(temp)/'source';(source/'os').mkdir(parents=True)
            recipe=b'unchanged frozen recipe';(source/'os/Containerfile').write_bytes(recipe)
            with patch.object(d, 'REPO', d.REPO), patch.object(d.subprocess,'check_output',side_effect=['c'*40,'',d.RECONSTRUCTION_SOURCE,'']):
                receipt=d.select_source(source,d.RECONSTRUCTION_SOURCE,'c'*40)
                self.assertEqual(d.REPO,source)
                self.assertEqual(receipt['source_recipe_sha256'],d.hashlib.sha256(recipe).hexdigest())
                self.assertEqual((source/'os/Containerfile').read_bytes(),recipe)
            for outputs in (['d'*40,''], ['c'*40,'dirty'], ['c'*40,'','wrong','']):
                with patch.object(d.subprocess,'check_output',side_effect=outputs), self.assertRaises(ValueError):
                    d.select_source(source,d.RECONSTRUCTION_SOURCE,'c'*40)

    def test_resume_hook_private_inputs_and_explicit_producer_only(self):
        with tempfile.TemporaryDirectory() as temp, patch.dict(os.environ,{'GITHUB_RUN_ID':'123','GITHUB_RUN_ATTEMPT':'1'}), \
             patch.object(d,'PRIVATE',Path(temp)), patch.object(d,'run') as run:
            args=SimpleNamespace(resume_checkpoint=[['/verified/partial.json','a'*64]],
                                 resume_thin_cache_approval=None, resume_reconstructed_partial_admission=None)
            d.resume_selected_native(args,{'source':'frozen'},{'PUBLIC_SETTING':'value'})
            command=run.call_args.args
            self.assertEqual(command[:2],('python3',str(d.CONTROLLER/'os/dev/resume-native-payload.py')))
            self.assertNotIn('build-image.sh',' '.join(command))
            self.assertEqual(command[-3:],('--checkpoint','/verified/partial.json','a'*64))
            for name in ('resume-inputs.json','resume-environment.json'):
                self.assertEqual((Path(temp)/name).stat().st_mode & 0o777,0o600)
            self.assertEqual(json.loads((Path(temp)/'resume-inputs.json').read_text()),{'source':'frozen'})


if __name__=='__main__': unittest.main()
