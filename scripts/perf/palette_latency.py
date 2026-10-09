"""Offline physical palette witnesses: exact draw, DRM retirement and GPU ownership.

No synthetic Wayland feedback is manufactured. CPU wall intervals are kept
separate from device-relative GPU intervals; this does not certify 120 Hz.
"""
import argparse
import json
import math
import os
import re
import uuid
from pathlib import Path

from compositor_phases import (BYTES, COSTS, MAX_RECORDS, capture_lines,
                               gpu_submission_records, integer, submission_records)
from runtime_benchmark import distribution
from settings_effects import summarize_settings_activation

PREFIXES = ('tilo-palette-input ', 'tilo-palette-frame ', 'tilo-action-submission ')
MODES = ('open', 'query', 'hover', 'click')
HASH = re.compile(r'[0-9a-f]{64}\Z')


def number(value, minimum=0):
    if type(value) is not int or not minimum <= value <= (1 << 64) - 1:
        raise ValueError('Missing or invalid bounded palette integer')
    return value


def random_id(value):
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError, TypeError):
        raise ValueError('Canonical random observer/invocation UUID required') from None
    if str(parsed) != value or parsed.version != 4:
        raise ValueError('Canonical random observer/invocation UUID required')
    return value


def digest(value):
    if not isinstance(value, str) or not HASH.fullmatch(value):
        raise ValueError('Expected BLAKE3 query/revision hash required')
    return value


def json_rows(lines, prefix):
    result = []
    for line in lines:
        _, found, payload = line.partition(prefix)
        if not found:
            continue
        if len(result) >= MAX_RECORDS or len(payload.encode()) > 65536:
            raise ValueError('Palette trace exceeds its row bound')
        def object_fields(pairs):
            row = {}
            for key, value in pairs:
                if key in row:
                    raise ValueError('Duplicate JSON trace field')
                row[key] = value
            return row
        row = json.loads(payload, object_pairs_hook=object_fields)
        if not isinstance(row, dict):
            raise ValueError('Palette trace must be an object')
        result.append(row)
    return result


def snapshot(value):
    if not isinstance(value, dict):
        raise ValueError('Actual palette snapshot required')
    for key in ('visible', 'mounted', 'query_current'):
        if type(value.get(key)) is not bool:
            raise ValueError('Palette visibility/publication witness missing')
    digest(value.get('query_hash'))
    number(value.get('query_bytes'))
    number(value.get('results_generation'))
    revision = value.get('published_revision_hash')
    if revision is not None:
        digest(revision)
    for key in ('selected_entry_id', 'hovered_entry_id'):
        if value.get(key) is not None and not isinstance(value[key], str):
            raise ValueError('Invalid palette entry identity')
    rows = value.get('rows')
    if not isinstance(rows, list) or len(rows) > 256:
        raise ValueError('Actual bounded palette rows required')
    seen = set()
    for row in rows:
        if not isinstance(row, dict):
            raise ValueError('Invalid palette row')
        for key in ('entry_id', 'provider_id', 'action_id'):
            if not isinstance(row.get(key), str) or not row[key]:
                raise ValueError('Missing palette row identity')
        if row['entry_id'] in seen:
            raise ValueError('Duplicate palette row identity')
        seen.add(row['entry_id'])
        bounds = row.get('bounds')
        if (not isinstance(bounds, list) or len(bounds) != 4
                or any(type(v) not in (int, float) or not math.isfinite(v) for v in bounds)
                or bounds[2] <= bounds[0] or bounds[3] <= bounds[1]):
            raise ValueError('Invalid mounted palette row bounds')
        for flag, identity in (('selected', 'selected_entry_id'), ('hovered', 'hovered_entry_id')):
            if type(row.get(flag)) is not bool or row[flag] != (value.get(identity) == row['entry_id']):
                raise ValueError('Drawn palette row identity disagrees with state')
    return value


def target_row(state, provider, action):
    rows = [r for r in state['rows'] if (r['provider_id'], r['action_id']) == (provider, action)]
    if len(rows) != 1:
        raise ValueError('Exact target provider/action is not uniquely drawn')
    return rows[0]


def lifecycles(lines):
    """Read actual DRM retirements directly, without client-feedback substitution."""
    result, proofs = {}, set()
    for line in lines:
        _, found, payload = line.partition('FRAME-LIFECYCLE ')
        if not found or not payload.startswith('serial='):
            continue
        if len(result) >= MAX_RECORDS or len(payload) > 4096:
            raise ValueError('Compositor lifecycle exceeds its bound')
        row = {}
        for token in payload.split():
            key, separator, value = token.partition('=')
            if not separator or key in row or '=' in value:
                raise ValueError('Invalid or duplicate compositor lifecycle field')
            row[key] = value
        serial = integer(row.get('serial'))
        if not serial or serial in result or row.get('event') != 'render-retired':
            raise ValueError('Duplicate or invalid compositor retirement')
        revision = number(integer(row.get('revision')))
        costs = {key: integer(row.get(key)) for key in COSTS}
        byte_counts = {key: integer(row.get(key)) for key in BYTES}
        if row.get('phase_timing') != 'true' or costs['total_us'] < costs['render_us'] + costs['queue_wait_us']:
            raise ValueError('Incomplete or unordered compositor CPU phases')
        stamp, sequence = row.get('physical_presented_time_ns'), row.get('physical_sequence')
        proof = None
        if stamp != 'none' or sequence != 'none':
            proof = (number(integer(stamp), 1), number(integer(sequence)))
            if proof in proofs:
                raise ValueError('Ambiguous physical KMS outcome')
            proofs.add(proof)
        result[serial] = dict(serial=serial, revision=revision, physical_outcome=proof,
                              costs_us=costs, byte_counts=byte_counts)
    return result


def compositor_evidence(record, gpu, cpu, producer):
    serial = record['serial']
    evidence = gpu.get(serial)
    if (not evidence or not evidence['available'] or evidence['revision'] != record['revision']
            or evidence['process_id'] != producer):
        raise ValueError('Missing exact compositor producer/revision GPU execution evidence')
    nested = cpu.get(serial)
    if not nested or not nested['available']:
        raise ValueError('Missing exact CPU submission boundaries')
    if sum(nested['costs_us'][k] for k in ('scene_record_us', 'command_encode_us',
                                        'encoder_finish_us', 'queue_submit_us')) > record['costs_us']['encode_submit_us']:
        raise ValueError('Nested CPU submission costs exceed aggregate')
    return dict(record, submission_gpu=evidence, submission_cpu=nested)


def summarize_palette_latency(lines, *, mode, process_id, observer_session,
                              expected_query_hash, target_provider, target_action,
                              target_pane=None, settings_process_id=None,
                              settings_observer_session=None):
    if mode not in MODES:
        raise ValueError('Explicit palette workload mode required')
    producer = number(process_id, 1)
    owner = producer, random_id(observer_session)
    expected_query_hash = digest(expected_query_hash)
    if not all(isinstance(v, str) and v for v in (target_provider, target_action)):
        raise ValueError('Exact target provider and action required')
    inputs, frames = {}, {}
    for prefix, destination, key in ((PREFIXES[0], inputs, 'id'), (PREFIXES[1], frames, 'pipe_serial')):
        for row in json_rows(lines, prefix):
            if (type(row.get('schema')) is not int or row.get('schema') != 1
                    or type(row.get('clock_id')) is not int or row.get('clock_id') != 1
                    or (row.get('process_id'), row.get('observer_session')) != owner):
                raise ValueError('Foreign producer or unsupported palette clock/schema')
            identity = number(row.get(key), 1)
            if identity in destination:
                raise ValueError('Duplicate palette input/frame identity')
            destination[identity] = row
    if not inputs:
        raise ValueError('Missing physical palette inputs')
    phases, gpu, cpu = lifecycles(lines), gpu_submission_records(lines), submission_records(lines)
    samples, mapped, outcomes = [], set(), set()
    for input_id, event in inputs.items():
        kind = {'open': 'physical_key', 'query': 'physical_key',
                'hover': 'physical_pointer_motion', 'click': 'physical_pointer_button'}[mode]
        if event.get('kind') != kind:
            raise ValueError('Wrong physical input kind for workload')
        start, dispatch, returned = [number(event.get(k), 1) for k in
                                     ('input_time_ns', 'dispatch_start_ns', 'dispatch_end_ns')]
        if not start <= dispatch <= returned:
            raise ValueError('Invalid physical dispatch clock ordering')
        before, after = snapshot(event.get('before')), snapshot(event.get('after'))
        if mode == 'click':
            row = target_row(before, target_provider, target_action)
            if (not before['visible'] or not before['mounted'] or not before['query_current']
                    or before['query_hash'] != expected_query_hash or not row['hovered']):
                raise ValueError('Click did not target the actual published hovered result')
            submission = [r for r in json_rows(lines, PREFIXES[2])
                          if r.get('palette_input_id') == input_id
                          and r.get('palette_observer_session') == observer_session]
            if (len(submission) != 1 or submission[0].get('source') != 'physical_pointer_button'
                    or submission[0].get('process_id') != producer
                    or submission[0].get('input_time_ns') != start
                    or submission[0].get('activation_dispatch_ns') != dispatch):
                raise ValueError('Missing original physical button/broker invocation join')
            invocation = random_id(submission[0].get('invocation'))
            activation = summarize_settings_activation(lines, invocation, target_pane,
                settings_process_id, settings_observer_session)
            if not activation['physical_activation_available']:
                raise ValueError('Broker-only activation is not physical button latency')
            matches = [r for r in phases.values() if r['physical_outcome'] ==
                       (activation['presented_time_ns'], activation['physical_sequence'])]
            if len(matches) != 1:
                raise ValueError('Missing exact Settings activation KMS retirement')
            evidence = compositor_evidence(matches[0], gpu, cpu, producer)
            end = activation['presented_time_ns']
            stages = dict(input_to_dispatch_ns=dispatch-start, dispatch_cpu_ns=returned-dispatch,
                          dispatch_end_to_settings_kms_ns=end-returned)
            sample = dict(input_id=input_id, invocation=invocation, activation=activation)
        else:
            candidates = [r for r in frames.values() if input_id in r.get('input_ids', [])]
            if (len(candidates) != 1 or candidates[0].get('input_ids') != [input_id]
                    or type(candidates[0]['input_ids'][0]) is not int):
                raise ValueError('Missing, duplicate or coalesced palette input-to-frame mapping')
            frame = candidates[0]
            serial = frame['pipe_serial']
            if serial in mapped:
                raise ValueError('Palette frame credited more than once')
            mapped.add(serial)
            revision = number(frame.get('scene_revision'))
            state = snapshot(frame.get('state'))
            if (not state['visible'] or not state['mounted'] or not state['query_current']
                    or state['query_hash'] != expected_query_hash
                    or state['published_revision_hash'] is None):
                raise ValueError('Actual draw has stale/unmounted query results')
            target = target_row(state, target_provider, target_action)
            if after['query_current'] and (
                    state['published_revision_hash'] != after['published_revision_hash']
                    or state['results_generation'] != after['results_generation']):
                raise ValueError('Drawn publication differs from the input-adopted current contract')
            if mode == 'open':
                if before['visible'] or not after['visible']:
                    raise ValueError('Input did not open the palette')
            elif mode == 'query':
                if (not before['visible'] or not before['mounted']
                        or not after['visible'] or not after['mounted']
                        or before['query_hash'] == expected_query_hash
                        or after['query_hash'] != expected_query_hash
                        or state['results_generation'] < after['results_generation']):
                    raise ValueError('Query was not adopted and published for this input')
            else:
                if (not all(s['visible'] and s['mounted'] and s['query_current'] for s in (before, after))
                        or before['hovered_entry_id'] == target['entry_id']
                        or after['hovered_entry_id'] != target['entry_id'] or not target['hovered']
                        or before['query_hash'] != expected_query_hash
                        or after['query_hash'] != expected_query_hash
                        or before['published_revision_hash'] != state['published_revision_hash']
                        or before['results_generation'] != state['results_generation']):
                    raise ValueError('Physical motion did not change the actual painted target hover')
            draw, submit = [number(frame.get(k)) for k in ('draw_time_ns', 'submit_time_ns')]
            record = phases.get(serial)
            if not record or record['revision'] != revision or not record['physical_outcome']:
                raise ValueError('No exact positive KMS retirement for palette frame')
            evidence = compositor_evidence(record, gpu, cpu, producer)
            end, sequence = record['physical_outcome']
            if not returned <= draw <= submit <= end:
                raise ValueError('Palette draw/submit/presentation clock ordering invalid')
            stages = dict(input_to_dispatch_ns=dispatch-start, dispatch_cpu_ns=returned-dispatch,
                          dispatch_end_to_draw_ns=draw-returned, draw_to_submit_ns=submit-draw,
                          submit_to_kms_ns=end-submit)
            sample = dict(input_id=input_id, pipe_serial=serial, scene_revision=revision,
                          target_entry_id=target['entry_id'], results_generation=state['results_generation'])
        proof = evidence['physical_outcome']
        if proof in outcomes or end < returned:
            raise ValueError('Reused or unordered palette KMS outcome')
        outcomes.add(proof)
        samples.append(dict(sample, input_time_ns=start, presented_time_ns=end,
                            latency_ns=end-start, stages_ns=stages, compositor=evidence))
    if mode != 'click' and mapped != set(frames):
        raise ValueError('Unmatched/stale palette draw frame in selected capture')
    ordered = sorted(samples, key=lambda sample: sample['input_id'])
    for previous, current in zip(ordered, ordered[1:]):
        if (previous['input_time_ns'] >= current['input_time_ns']
                or previous['presented_time_ns'] >= current['presented_time_ns']
                or previous['compositor']['physical_outcome'][1] >= current['compositor']['physical_outcome'][1]):
            raise ValueError('Non-monotonic input/presentation sample sequence')
    stage_names = samples[0]['stages_ns']
    return dict(available=True, mode=mode, process_id=producer, observer_session=observer_session,
        expected_query_hash=expected_query_hash, target_provider=target_provider, target_action=target_action,
        samples=len(samples), frames=samples, latency=distribution([r['latency_ns'] for r in samples]),
        stages={key: distribution([r['stages_ns'][key] for r in samples]) for key in stage_names},
        compositor_cpu={key: distribution([r['compositor']['costs_us'][key] for r in samples], scale=.001)
                        for key in COSTS},
        compositor_gpu={key: distribution([r['compositor']['submission_gpu'][key] for r in samples])
                        for key in ('recording_ns', 'post_render_ns')},
        performance_certified=False, limitations=[
            'CPU intervals include scheduling/logging overhead; completion wait is not GPU execution.',
            'GPU durations are device-relative, exclude queue wait/query resolve, and nested intervals overlap.',
            'This capture does not establish sustained native-resolution 120 Hz or device-to-photon latency.',
            'Caller must pin the authenticated session, worker, source cohort and runtime before capture.'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--log', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--mode', choices=MODES, required=True)
    parser.add_argument('--process-id', type=int, required=True)
    parser.add_argument('--observer-session', required=True)
    parser.add_argument('--expected-query-hash', required=True)
    parser.add_argument('--target-provider', required=True)
    parser.add_argument('--target-action', required=True)
    parser.add_argument('--target-pane')
    parser.add_argument('--settings-process-id', type=int)
    parser.add_argument('--settings-observer-session')
    args = parser.parse_args()
    options = vars(args).copy()
    log, out = options.pop('log'), options.pop('out')
    if args.mode == 'click' and any(options[key] is None for key in
            ('target_pane', 'settings_process_id', 'settings_observer_session')):
        parser.error('click requires exact Settings pane and worker PID/observer session')
    report = summarize_palette_latency(capture_lines(log), **options)
    descriptor = os.open(out, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(descriptor, 'w') as destination:
        json.dump(report, destination, indent=2)
        destination.write('\n')


if __name__ == '__main__':
    main()
