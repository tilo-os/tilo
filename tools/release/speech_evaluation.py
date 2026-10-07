#!/usr/bin/env python3
"""Freeze public speech comparisons and score backend-neutral streamed results.

No decoder, microphone, model download, signing, or installation happens here.
All previously used first eight archive members remain development data.
Reserved members 8..10 must stay uninspected until configurations are frozen.
"""
import argparse
import hashlib
import json
import math
import re
from pathlib import Path
import statistics
import wave

from prepare_speech_benchmark import LANGUAGES, REVISION, stage_language
from probe_speech_stream import synthetic_noise, verified
from summarize_speech_benchmark import distance, units

NORMALIZATION = 'NFKC/casefold/punctuation-to-space; no numeric/accent/script rewrite'
REQUIRED_COVERAGE = ['natural_noise', 'natural_code_switch', 'spoken_numeric_self_correction',
                     'unseen_spoken_names', 'physical_older_device']


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False,
                                     separators=(',', ':')).encode()).hexdigest()


def read_json(path):
    if path.stat().st_size > 4 * 1024 * 1024:
        raise ValueError('evaluation metadata exceeds bound')
    return json.loads(path.read_text())


def prepare(destination):
    destination.mkdir(mode=0o700, parents=True, exist_ok=False)
    manifest = {'dataset': 'google/fleurs', 'revision': REVISION, 'license': 'CC-BY-4.0',
                'attribution': 'Google FLEURS dataset authors', 'cases': []}
    for config, language in LANGUAGES:
        manifest['cases'].extend(stage_language(destination, config, language, 11))
    (destination / 'manifest.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')


def freeze(manifest, root, destination):
    if manifest.get('dataset') != 'google/fleurs' or manifest.get('revision') != REVISION:
        raise ValueError('unexpected pinned public corpus')
    indexed = {case['id']: case for case in manifest['cases']}
    if len(indexed) != len(manifest['cases']):
        raise ValueError('duplicate source cases')
    development, heldout = set(), set()
    for config, language in LANGUAGES:
        for index in range(11):
            case = indexed.get(f'{config}-{index}')
            if case is None or case['language'] != language:
                raise ValueError('all fixed development and reserved members required')
            path = verified(root, case)
            # References, original audio and converted audio all forbid leakage.
            keys = {('pcm', case['sha256']), ('source', case['source_sha256']),
                    ('reference', language, tuple(units(case['reference'], language == 'zh')))}
            (development if index < 8 else heldout).update(keys)
            with wave.open(str(path)) as audio:
                if (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) != (1, 2, 16000):
                    raise ValueError('fixed sources require mono PCM16 16 kHz')
    if development & heldout:
        raise ValueError('reserved sources overlap previously exposed data; revise the plan before decoding')
    destination.mkdir(mode=0o700, parents=True, exist_ok=False)
    cases = []

    def add(split, scenario, sources, language, snr=None, vocabulary=None):
        receipts = [indexed[source] for source in sources]
        chunks = []
        for receipt in receipts:
            with wave.open(str(verified(root, receipt))) as audio:
                chunks.append(audio.readframes(audio.getnframes()))
        pcm = b''.join(chunks)
        if not 0 < len(pcm) <= 120 * 32000:
            raise ValueError('public comparison exceeds session bound')
        noise = None
        if snr is not None:
            pcm, noise = synthetic_noise(pcm, snr)
        case_id = f'{split}-{scenario}-' + '+'.join(sources)
        filename = case_id + '.wav'
        with wave.open(str(destination / filename), 'wb') as audio:
            audio.setparams((1, 2, 16000, 0, 'NONE', 'not compressed'))
            audio.writeframes(pcm)
        path = destination / filename
        cases.append({'id': case_id, 'split': split, 'scenario': scenario, 'language': language,
                      'sources': [{key: receipt[key] for key in
                                   ('id', 'sha256', 'source_sha256', 'transcription_sha256')}
                                  for receipt in receipts],
                      'file': filename, 'size': path.stat().st_size,
                      'sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
                      'input_pcm_sha256': hashlib.sha256(pcm).hexdigest(),
                      'reference': ' '.join(receipt['reference'] for receipt in receipts),
                      'audio_seconds': len(pcm) / 32000, 'synthetic_noise': noise,
                      'vocabulary': vocabulary or [], 'normalization': NORMALIZATION})

    for split, indices in [('development', [0]), ('holdout', [8, 9, 10])]:
        for config, language in LANGUAGES:
            for index in indices:
                add(split, 'read', [f'{config}-{index}'], language)
            for snr in (20, 10):
                add(split, f'synthetic-noise-{snr}db', [f'{config}-{indices[0]}'], language, snr)
        for left, right in [('en_us', 'de_de'), ('fr_fr', 'es_419')]:
            add(split, 'assembled-transition', [f'{left}-{indices[0]}', f'{right}-{indices[0]}'], 'auto')
    # Predeclared from previously exposed development text, never holdout references.
    add('development', 'known-name-bias', ['de_de-0'], 'de', vocabulary=['Hongkong', 'Kowloon', 'TiLo'])
    suite = {'schema': 1, 'dataset': manifest['dataset'], 'revision': REVISION,
             'license': manifest['license'], 'attribution': manifest['attribution'],
             'split_policy': 'archive indices 0..7 exposed; 8..10 reserved, never tune on holdout',
             'selection': 'fixed archive order, no model-error-based selection',
             'holdout_status': 'reserved, not decoded by this tool; exposure requires an operator ledger',
             'required_missing_coverage': REQUIRED_COVERAGE, 'cases': cases,
             'numeric_repair_text_only': [
                 {'language': 'en', 'raw': 'Take two, uh, no, three.', 'cleaned': 'Take three.'},
                 {'language': 'de', 'raw': 'Nimm zwei, äh, nein, drei.', 'cleaned': 'Nimm drei.'},
                 {'language': 'fr', 'raw': 'Prendre deux, euh, non, trois.', 'cleaned': 'Prendre trois.'},
                 {'language': 'es', 'raw': 'Toma dos, eh, no, tres.', 'cleaned': 'Toma tres.'}],
             'numeric_audio_status': 'not recorded; text-only cleanup fixtures are not ASR accuracy evidence'}
    suite['suite_sha256'] = digest(suite)
    (destination / 'suite.json').write_text(json.dumps(suite, ensure_ascii=False, indent=2) + '\n')
    return suite


def finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value) and value >= 0


def stream_metrics(row, language):
    drafts = row['drafts']
    if not isinstance(drafts, list) or len(drafts) > 1500:
        raise ValueError('bounded draft timeline required')
    final = row['final_text']
    if not isinstance(final, str) or len(final.encode()) > 32768:
        raise ValueError('bounded public final required')
    if not drafts:
        raise ValueError('streaming comparison requires an observed draft timeline')
    prior_time, prior_samples = -1, -1
    for draft in drafts:
        if (not isinstance(draft['text'], str) or len(draft['text'].encode()) > 32768
                or not finite(draft['wall_ms']) or draft['wall_ms'] < prior_time
                or not isinstance(draft['samples'], int) or isinstance(draft['samples'], bool)
                or not 0 <= draft['samples'] <= 120 * 16000 or draft['samples'] < prior_samples):
            raise ValueError('invalid or nonmonotonic draft timeline')
        prior_time, prior_samples = draft['wall_ms'], draft['samples']
    first = next((draft['wall_ms'] for draft in drafts if draft['text']), None)
    stable, stable_units, meaningful = None, None, None
    for index, draft in enumerate(drafts):
        text = draft['text']
        if text and final.startswith(text) and all(later['text'].startswith(text) for later in drafts[index + 1:]):
            count = len(units(text, language == 'zh'))
            if stable is None:
                stable, stable_units = draft['wall_ms'], count
            if meaningful is None and count >= (6 if language == 'zh' else 3):
                meaningful = draft['wall_ms']
    revisions = sum(bool(left['text']) and not right['text'].startswith(left['text'])
                    for left, right in zip(drafts, drafts[1:]))
    if drafts and drafts[-1]['text'] and not final.startswith(drafts[-1]['text']):
        revisions += 1
    return {'first_partial_ms': first, 'first_stable_partial_ms': stable,
            'first_stable_partial_units': stable_units, 'first_meaningful_stable_partial_ms': meaningful,
            'non_prefix_revisions_including_final': revisions,
            'stability_definition': 'retrospective exact prefix of every later draft and final; not a causal guarantee'}


def summarize(suite, rows, split):
    payload = dict(suite)
    stated = payload.pop('suite_sha256')
    if digest(payload) != stated:
        raise ValueError('suite digest mismatch')
    expected = {case['id']: case for case in suite['cases'] if case['split'] == split}
    if not expected or len(rows) != len(expected) or {row['case'] for row in rows} != set(expected):
        raise ValueError('missing, duplicated, or wrong-split results')
    engine = rows[0]['engine']
    if not isinstance(engine, dict) or any(not engine.get(key) for key in
                                         ('id', 'revision', 'license_receipt_sha256', 'model_sha256',
                                          'adapter_sha256', 'configuration_sha256', 'profile')):
        raise ValueError('exact engine/model/license/configuration/profile receipts required')
    for key in ('license_receipt_sha256', 'model_sha256', 'adapter_sha256', 'configuration_sha256'):
        if not isinstance(engine[key], str) or not re.fullmatch('[0-9a-f]{64}', engine[key]):
            raise ValueError('exact SHA-256 engine receipts required')
    metrics = []
    for row in rows:
        case = expected[row['case']]
        if (row['engine'] != engine or row['suite_sha256'] != stated
                or row['input_pcm_sha256'] != case['input_pcm_sha256']):
            raise ValueError('mixed engines, configurations, or mismatched input')
        if row['status'] != 'success':
            raise ValueError('failed cases cannot disappear from comparison or become acceptance')
        if not all(finite(row[key]) for key in ('finalization_ms', 'cpu_seconds', 'peak_rss_kib',
                                                'peak_address_space_kib', 'wall_seconds')):
            raise ValueError('observed finite resource/finalization metrics required')
        if row['peak_rss_kib'] <= 0 or row['peak_address_space_kib'] < row['peak_rss_kib']:
            raise ValueError('unobserved or inconsistent process memory')
        if any(draft['samples'] > round(case['audio_seconds'] * 16000) for draft in row['drafts']):
            raise ValueError('draft samples exceed the exact public case')
        characters = case['language'] == 'zh'
        reference = units(case['reference'], characters)
        if not reference:
            raise ValueError('empty reference')
        errors = distance(reference, units(row['final_text'], characters))
        metrics.append(dict(case=case['id'], language=case['language'], scenario=case['scenario'],
                            error_metric='CER' if characters else 'WER', errors=errors,
                            reference_units=len(reference), error_fraction=errors / len(reference),
                            **stream_metrics(row, case['language']),
                            **{key: row[key] for key in ('finalization_ms', 'cpu_seconds', 'peak_rss_kib',
                                                        'peak_address_space_kib', 'wall_seconds')}))
    groups = {}
    for metric in metrics:
        key = metric['language'] + '/' + metric['scenario']
        groups.setdefault(key, []).append(metric)
    aggregates = {key: {'cases': len(group), 'error_metric': group[0]['error_metric'],
                        'error_fraction': sum(row['errors'] for row in group) /
                        sum(row['reference_units'] for row in group),
                        'median_finalization_ms': statistics.median(row['finalization_ms'] for row in group),
                        'max_peak_rss_kib': max(row['peak_rss_kib'] for row in group)}
                  for key, group in groups.items()}
    return {'schema': 1, 'suite_sha256': stated, 'split': split, 'engine': engine,
            'normalization': NORMALIZATION, 'cases': metrics, 'groups': aggregates,
            'required_missing_coverage': suite['required_missing_coverage'], 'release_qualified': False}


def import_stream(suite, log, engine, split):
    if log.stat().st_size > 16 * 1024 * 1024:
        raise ValueError('public probe log exceeds bound')
    records = []
    for line in log.read_text().splitlines():
        if line.startswith('SPEECH_STREAM_FAILURE '):
            raise ValueError('failed public probe retained; comparison is incomplete')
        if not line.startswith('SPEECH_STREAM_PROBE '):
            continue
        probe = json.loads(line.split(' ', 1)[1])
        if probe['model_sha256'] != engine['model_sha256'] or probe['worker_exit'] != 0:
            raise ValueError('mismatched model or failed worker')
        records.append({'case': probe['case'], 'suite_sha256': suite['suite_sha256'],
                        'input_pcm_sha256': probe['input_pcm_sha256'], 'engine': engine,
                        'status': 'success', 'drafts': probe['drafts'], 'final_text': probe['final_text'],
                        'finalization_ms': probe['finalization_ms'],
                        'cpu_seconds': probe['cpu_user_seconds'] + probe['cpu_system_seconds'],
                        'peak_rss_kib': probe['memory_peak']['VmHWM'],
                        'peak_address_space_kib': probe['memory_peak']['VmPeak'],
                        'wall_seconds': probe['wall_seconds']})
    summarize(suite, records, split)  # Validate before publishing any converted result.
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    stage = commands.add_parser('prepare-public')
    stage.add_argument('destination', type=Path)
    build = commands.add_parser('freeze')
    build.add_argument('manifest', type=Path)
    build.add_argument('destination', type=Path)
    score = commands.add_parser('score')
    score.add_argument('suite', type=Path)
    score.add_argument('results', type=Path)
    score.add_argument('--split', choices=('development', 'holdout'), required=True)
    convert = commands.add_parser('import-stream')
    convert.add_argument('suite', type=Path)
    convert.add_argument('log', type=Path)
    convert.add_argument('engine', type=Path)
    convert.add_argument('destination', type=Path)
    convert.add_argument('--split', choices=('development', 'holdout'), required=True)
    args = parser.parse_args()
    if args.command == 'prepare-public':
        prepare(args.destination)
    elif args.command == 'freeze':
        suite = freeze(read_json(args.manifest), args.manifest.parent, args.destination)
        print(json.dumps({'suite_sha256': suite['suite_sha256'], 'cases': len(suite['cases']),
                          'required_missing_coverage': suite['required_missing_coverage']}))
    elif args.command == 'import-stream':
        results = import_stream(read_json(args.suite), args.log, read_json(args.engine), args.split)
        with args.destination.open('x') as output:
            output.write(json.dumps(results, ensure_ascii=False, indent=2) + '\n')
    else:
        print(json.dumps(summarize(read_json(args.suite), read_json(args.results), args.split),
                         ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
