"""Fixed-corpus leakage checks and independent streamed comparison metrics."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest
import wave

RELEASE = Path(__file__).resolve().parents[1] / 'release'
sys.path.insert(0, str(RELEASE))
spec = importlib.util.spec_from_file_location('speech_evaluation', RELEASE / 'speech_evaluation.py')
evaluation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluation)


def suite_and_row(language='en'):
    case = {'id': 'test', 'split': 'development', 'language': language, 'scenario': 'read',
            'reference': 'one two three' if language == 'en' else '这是中文',
            'input_pcm_sha256': 'a' * 64, 'audio_seconds': 10}
    suite = {'cases': [case], 'required_missing_coverage': ['natural_noise']}
    suite['suite_sha256'] = evaluation.digest(suite)
    engine = dict(id='fixture', revision='pinned', license_receipt_sha256='b' * 64,
                  model_sha256='c' * 64, adapter_sha256='d' * 64,
                  configuration_sha256='e' * 64, profile='synthetic test only')
    row = dict(case='test', suite_sha256=suite['suite_sha256'], engine=engine,
               input_pcm_sha256='a' * 64, status='success', drafts=[
                   {'samples': 16000, 'wall_ms': 1100, 'text': 'wrong'},
                   {'samples': 32000, 'wall_ms': 2300, 'text': 'one'},
                   {'samples': 48000, 'wall_ms': 3400, 'text': 'one two three'}],
               final_text='one two three', finalization_ms=10, cpu_seconds=3,
               peak_rss_kib=100, peak_address_space_kib=200, wall_seconds=11)
    return suite, row


class SpeechEvaluationTests(unittest.TestCase):
    def test_first_visible_is_distinct_from_stable_and_includes_final_revision(self):
        suite, row = suite_and_row()
        result = evaluation.summarize(suite, [row], 'development')['cases'][0]
        self.assertEqual(result['first_partial_ms'], 1100)
        self.assertEqual(result['first_stable_partial_ms'], 2300)
        self.assertEqual(result['first_stable_partial_units'], 1)
        self.assertEqual(result['first_meaningful_stable_partial_ms'], 3400)
        self.assertEqual(result['non_prefix_revisions_including_final'], 1)
        row['final_text'] = 'different final'
        result = evaluation.summarize(suite, [row], 'development')['cases'][0]
        self.assertIsNone(result['first_stable_partial_ms'])
        self.assertEqual(result['non_prefix_revisions_including_final'], 2)

    def test_language_specific_errors_do_not_rewrite_numbers_or_chinese(self):
        suite, row = suite_and_row('zh')
        row['final_text'] = '這是中文'
        result = evaluation.summarize(suite, [row], 'development')
        self.assertEqual(result['cases'][0]['error_metric'], 'CER')
        self.assertEqual(result['cases'][0]['error_fraction'], .25)
        self.assertFalse(result['release_qualified'])
        suite, row = suite_and_row()
        row['final_text'] = '1 two three'
        self.assertAlmostEqual(evaluation.summarize(suite, [row], 'development')['cases'][0]['error_fraction'], 1/3)

    def test_incomplete_failed_wrong_split_and_stale_results_fail(self):
        suite, row = suite_and_row()
        for rows, split in [([], 'development'), ([row, row], 'development'), ([row], 'holdout')]:
            with self.assertRaises(ValueError):
                evaluation.summarize(suite, rows, split)
        for key, value in [('status', 'failed'), ('suite_sha256', 'f' * 64),
                           ('input_pcm_sha256', 'f' * 64), ('cpu_seconds', float('nan')),
                           ('peak_rss_kib', -1)]:
            changed = dict(row, **{key: value})
            with self.assertRaises(ValueError):
                evaluation.summarize(suite, [changed], 'development')
        modified = copy.deepcopy(suite)
        modified['cases'][0]['reference'] = 'easier reference'
        with self.assertRaises(ValueError):
            evaluation.summarize(modified, [row], 'development')

    def test_missing_or_nonmonotonic_stream_cannot_claim_stability(self):
        suite, row = suite_and_row()
        for drafts in [[], [{'samples': 1, 'wall_ms': -1, 'text': 'one'}],
                       [{'samples': 2, 'wall_ms': 10, 'text': 'one'},
                        {'samples': 1, 'wall_ms': 11, 'text': 'one two'}],
                       [{'samples': 160001, 'wall_ms': 11, 'text': 'one'}]]:
            with self.assertRaises(ValueError):
                evaluation.summarize(suite, [dict(row, drafts=drafts)], 'development')

    def test_failed_stream_log_is_not_imported_as_success(self):
        suite, row = suite_and_row()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'log'
            path.write_text('SPEECH_STREAM_FAILURE {"worker_error":"recognition"}\n')
            with self.assertRaises(ValueError):
                evaluation.import_stream(suite, path, row['engine'], 'development')

    def test_fixed_plan_reproduces_noise_and_rejects_exposed_holdout(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = {'dataset': 'google/fleurs', 'revision': evaluation.REVISION,
                        'license': 'CC-BY-4.0', 'attribution': 'fixture', 'cases': []}
            for language_index, (config, language) in enumerate(evaluation.LANGUAGES):
                for index in range(11):
                    path = root / f'{config}-{index}.wav'
                    pcm = struct.pack('<' + 'h' * 160, *([1000 + index + language_index * 20] * 160))
                    with wave.open(str(path), 'wb') as audio:
                        audio.setparams((1, 2, 16000, 0, 'NONE', 'not compressed'))
                        audio.writeframes(pcm)
                    sha = hashlib.sha256(path.read_bytes()).hexdigest()
                    manifest['cases'].append(dict(id=f'{config}-{index}', language=language,
                        file=path.name, size=path.stat().st_size, sha256=sha,
                        source_sha256=sha, transcription_sha256='f' * 64,
                        reference=f'public {language} reference {index}'))
            left = evaluation.freeze(manifest, root, root / 'left')
            right = evaluation.freeze(manifest, root, root / 'right')
            self.assertEqual(left, right)
            self.assertEqual(len(left['cases']), 53)
            self.assertEqual(sum(case['split'] == 'holdout' for case in left['cases']), 32)
            for case in left['cases']:
                self.assertEqual((root / 'left' / case['file']).read_bytes(),
                                 (root / 'right' / case['file']).read_bytes())
            self.assertEqual(left['numeric_audio_status'].split(';')[0], 'not recorded')
            manifest['cases'][8]['reference'] = manifest['cases'][0]['reference']
            with self.assertRaises(ValueError):
                evaluation.freeze(manifest, root, root / 'leaked')
            self.assertFalse((root / 'leaked').exists())


if __name__ == '__main__':
    unittest.main()
