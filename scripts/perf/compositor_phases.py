"""Bounded exact-KMS-outcome joins, not time-proximity or Notes-only attribution."""
from runtime_benchmark import distribution

COSTS = ('queue_wait_us', 'render_us', 'total_us', 'encode_submit_us',
         'gpu_completion_wait_us', 'bump_settle_us', 'backdrop_us',
         'kms_retirement_us', 'cpu_copy_us')
BYTES = ('readback_bytes', 'output_bytes')
MAX_RECORDS = 10000
MAX_CAPTURE_BYTES = 32 * 1024 * 1024


def capture_lines(path):
    with path.open("rb") as source:
        payload = source.read(MAX_CAPTURE_BYTES + 1)
    if len(payload) > MAX_CAPTURE_BYTES:
        raise ValueError("Phase capture exceeds its byte bound")
    try:
        return payload.decode("utf-8").splitlines()
    except UnicodeDecodeError:
        raise ValueError("Invalid phase capture encoding") from None


def integer(value):
    if not isinstance(value, str) or not value.isascii() or not value.isdecimal():
        raise ValueError('Invalid compositor phase integer')
    return int(value)


SUBMISSION_COUNTS = ('scene_count', 'command_count', 'finish_count', 'submit_count')
SUBMISSION_COSTS = ('scene_record_us', 'command_encode_us', 'encoder_finish_us', 'queue_submit_us')


def submission_records(lines):
    """Optional nested CPU costs, attached only to their exact presenter serial."""
    result = {}
    for line in lines:
        _, found, suffix = line.partition('FRAME-SUBMISSION ')
        if not found:
            continue
        if len(result) >= MAX_RECORDS or len(suffix) > 4096:
            raise ValueError('Submission boundary capture exceeds its bound')
        row = {}
        for token in suffix.split():
            key, separator, value = token.partition('=')
            if not separator or key in row:
                raise ValueError('Invalid submission boundary record')
            row[key] = value
        serial = integer(row.get('serial'))
        if not serial or serial in result:
            raise ValueError('Invalid or duplicate submission serial')
        status = row.get('status')
        if status not in ('complete', 'incomplete', 'invalid', 'missing'):
            raise ValueError('Invalid submission boundary status')
        counts = [integer(row.get(key)) for key in SUBMISSION_COUNTS]
        costs = {}
        for count, key in zip(counts, SUBMISSION_COSTS):
            value = row.get(key)
            if (count == 0) != (value == 'none'):
                raise ValueError('Missing submission boundary must not report measured zero')
            costs[key] = None if value == 'none' else integer(value)
        marker = row.get('sparse_marker_submit_us')
        costs['sparse_marker_submit_us'] = None if marker == 'none' else integer(marker)
        if status == 'complete' and (not counts[0] or len(set(counts)) != 1 or counts[0] > 256):
            raise ValueError('Incomplete submission boundaries claim completion')
        if status == 'missing' and any(counts):
            raise ValueError('Missing submission boundaries claim observations')
        result[serial] = dict(status=status, available=status == 'complete', counts=counts, costs_us=costs)
    return result


def gpu_submission_records(lines):
    """Exact recording ownership; nested output duration is never summed with its envelope."""
    result = {}
    owned = set()
    for line in lines:
        _, found, suffix = line.partition('FRAME-GPU-SUBMISSION ')
        if not found:
            continue
        if len(result) >= MAX_RECORDS or len(suffix) > 4096:
            raise ValueError('GPU submission capture exceeds its bound')
        row = {}
        for token in suffix.split():
            key, separator, value = token.partition('=')
            if not separator or key in row:
                raise ValueError('Invalid GPU submission field')
            row[key] = value
        serial, revision = integer(row.get('serial')), integer(row.get('revision'))
        if not serial or serial in result or row.get('scope') != 'encoder_before_query_resolve':
            raise ValueError('Invalid GPU submission identity/scope')
        status = row.get('status')
        if status not in ('complete', 'incomplete', 'unsupported', 'disabled', 'missing'):
            raise ValueError('Invalid GPU coverage status')
        process = integer(row.get('process_id'))
        if not process:
            raise ValueError('Missing actual GPU producer process')
        flags = {}
        for key in ('encoder_supported', 'encoder_enabled', 'external_queue_work'):
            if row.get(key) not in ('true', 'false'):
                raise ValueError('Invalid GPU capability metadata')
            flags[key] = row[key] == 'true'
        identity = lambda value: None if value == 'none' else integer(value)
        generation, first, last = [identity(row.get(key)) for key in
                                  ('renderer_generation', 'first_recording', 'last_recording')]
        count, measured, stale = [integer(row.get(key)) for key in
                                  ('recording_count', 'measured_count', 'stale_count')]
        if count > 256 or measured > count or stale > 256:
            raise ValueError('Unbounded or overlapping GPU recording ownership')
        if count and (not generation or not first or last is None or last < first or last - first + 1 != count):
            raise ValueError('Incomplete GPU recording range')
        if not count and (first is not None or last is not None):
            raise ValueError('Missing GPU range reports identities')
        recording, post = [identity(row.get(key)) for key in ('recording_ns', 'post_render_ns')]
        if (measured == 0) != (recording is None) or (measured == 0) != (post is None):
            raise ValueError('Unavailable GPU duration is not a measured zero')
        bounded = [serial, revision, process, generation, first, last, recording, post]
        if any(value is not None and value > (1 << 64) - 1 for value in bounded):
            raise ValueError('GPU identity or duration exceeds u64')
        if measured and post > recording:
            raise ValueError('Nested output GPU interval exceeds envelope')
        if status == 'complete' and (not count or measured != count or stale
                or not flags['encoder_supported'] or not flags['encoder_enabled'] or flags['external_queue_work']):
            raise ValueError('Partial GPU coverage claims completion')
        if status in ('unsupported', 'disabled', 'missing') and measured:
            raise ValueError('Unavailable GPU coverage claims observations')
        if any(item['process_id'] != process for item in result.values()):
            raise ValueError('Mixed compositor producer lifetimes')
        if count:
            identifiers = {(process, generation, identity) for identity in range(first, last + 1)}
            if owned.intersection(identifiers):
                raise ValueError('GPU recording reused across producer frames')
            owned.update(identifiers)
        result[serial] = dict(revision=revision, status=status, available=status == 'complete',
            process_id=process, capabilities=flags,
            renderer_generation=generation, first_recording=first, last_recording=last,
            recordings=count, measured=measured, stale=stale,
            recording_ns=recording, post_render_ns=post,
            excludes_pre_execution_queue=True, excludes_query_resolve=True)
    return result


def summarize_compositor_phases(lines, samples, feedback):
    submissions = submission_records(lines)
    gpu_submissions = gpu_submission_records(lines)
    records = []
    serials = set()
    proofs = {}
    for line in lines:
        _, found, suffix = line.partition('FRAME-LIFECYCLE ')
        if not found or not suffix.startswith('serial='):
            continue  # Backpressure records have no presented frame.
        if len(records) >= MAX_RECORDS or len(suffix) > 4096:
            raise ValueError('Compositor phase capture exceeds its bound')
        row = {}
        for token in suffix.split():
            if token.count('=') != 1:
                raise ValueError('Invalid compositor lifecycle record')
            key, value = token.split('=')
            if key in row:
                raise ValueError('Duplicate compositor lifecycle field')
            row[key] = value
        if row.get('event') != 'render-retired' or row.get('phase_timing') not in ('true', 'false'):
            raise ValueError('Invalid compositor lifecycle event')
        serial, revision = [integer(row.get(key)) for key in ('serial', 'revision')]
        if serial == 0 or serial in serials:
            raise ValueError('Duplicate compositor serial or mixed presenter lifetimes')
        serials.add(serial)
        costs = {key: integer(row[key]) for key in COSTS if key in row}
        if any(key not in costs for key in ('queue_wait_us', 'render_us', 'total_us')):
            raise ValueError('Missing compositor lifecycle duration')
        if costs['total_us'] < costs['queue_wait_us'] + costs['render_us']:
            raise ValueError('Invalid compositor lifecycle duration ordering')
        byte_counts = {key: integer(row[key]) for key in BYTES if key in row}
        phase_complete = row['phase_timing'] == 'true' and len(costs) == len(COSTS) and len(byte_counts) == len(BYTES)
        if row['phase_timing'] == 'false' and any(costs.get(key, 0) for key in COSTS[3:]):
            raise ValueError('Unavailable compositor phase reports measured costs')
        if row['phase_timing'] == 'false' and any(byte_counts.values()):
            raise ValueError('Unavailable compositor phase reports measured bytes')
        timestamp, sequence = [row.get(key, 'none') for key in
                               ('physical_presented_time_ns', 'physical_sequence')]
        if (timestamp == 'none') != (sequence == 'none'):
            raise ValueError('Partial physical compositor outcome')
        proof = None if timestamp == 'none' else (integer(timestamp), integer(sequence))
        work = None
        if 'render_work' in row:
            if row['render_work'] not in ('true', 'false') or row.get('damage_admitted') not in ('true', 'false'):
                raise ValueError('Invalid compositor render decision')
            counts = {key: integer(row.get(key)) for key in
                      ('target_width', 'target_height', 'render_attempts', 'sparse_attempts', 'full_attempts')}
            if row['render_work'] == 'true':
                if (not phase_complete or not counts['target_width'] or not counts['target_height']
                        or not counts['render_attempts'] or counts['render_attempts'] !=
                        counts['sparse_attempts'] + counts['full_attempts']):
                    raise ValueError('Incomplete compositor render decision')
                work = dict(counts, damage_admitted=row['damage_admitted'] == 'true')
            elif any(counts.values()) or row['damage_admitted'] != 'false':
                raise ValueError('Unavailable compositor render work reports decisions')
        record = dict(serial=serial, revision=revision, phase_samples_available=phase_complete,
                      costs_us=costs, byte_counts=byte_counts, physical_outcome=proof, render_work=work)
        record['submission_cpu'] = submissions.get(serial, dict(status='missing', available=False))
        record['submission_gpu'] = gpu_submissions.get(serial, dict(status='missing', available=False))
        gpu = record['submission_gpu']
        if 'revision' in gpu and gpu['revision'] != revision:
            raise ValueError('GPU recording belongs to another frame revision')
        if gpu['available'] and not phase_complete:
            raise ValueError('GPU coverage has no completed frame phases')
        if record['submission_cpu']['available']:
            nested = sum(record['submission_cpu']['costs_us'][key] for key in SUBMISSION_COSTS)
            if not phase_complete or nested > costs['encode_submit_us']:
                raise ValueError('Submission CPU boundaries exceed their aggregate')
        records.append(record)
        if proof is not None:
            if proof in proofs:
                raise ValueError('Ambiguous compositor physical outcome')
            proofs[proof] = record
    frames = []
    matched = set()
    for sample in samples:
        display = feedback[sample['cpu_frame_id']]
        if (display.get('clock_id') != 1 or display.get('discarded') is not False
                or type(display.get('flags')) is not int or display['flags'] & 7 != 7
                or type(display.get('presented_time_ns')) is not int
                or display['presented_time_ns'] < 0):
            raise ValueError('Compositor join requires verified monotonic hardware feedback')
        sequence = display.get('sequence')
        record = (proofs.get((display['presented_time_ns'], sequence))
                  if type(sequence) is int and sequence >= 0 else None)
        frame = dict(input_id=sample['input_id'], cpu_frame_id=sample['cpu_frame_id'],
                     compositor_match_available=record is not None)
        if record is not None:
            if record['serial'] in matched:
                raise ValueError('Compositor frame reused across changed Notes inputs')
            matched.add(record['serial'])
            frame.update(record)
            if record['phase_samples_available']:
                readback = record['byte_counts']['readback_bytes']
                frame['readback_classification'] = ('compositor_readback_reported' if readback
                                                    else 'no_compositor_readback_reported')
        frames.append(frame)
    measured = [row for row in frames if row.get('phase_samples_available')]
    return dict(available=bool(measured), performance_certified=False,
                coverage=dict(verified_notes_frames=len(samples), lifecycle_records=len(records),
                              exact_outcome_matches=len(matched), phase_samples_matched=len(measured),
                              unmatched_lifecycle_records=len(records) - len(matched),
                              records_without_physical_outcome=sum(row['physical_outcome'] is None for row in records)),
                complete=bool(samples) and len(measured) == len(samples), frames=frames,
                costs={key: distribution([row['costs_us'][key] for row in measured], scale=.001)
                       for key in COSTS} if measured else {},
                limitations=[
                    'Only exact kernel timestamp and sequence equality links a compositor frame to verified app feedback.',
                    'Matched compositor costs cover the whole scene, not Notes alone; intervals may overlap client stages.',
                    'Submission CPU boundaries are nested within encode_submit; the sparse marker is separate. Missing boundaries are unavailable.',
                    'Completion wait includes GPU waiting or readback mapping and is not GPU execution duration.',
                    'No compositor readback bytes does not prove absence of copies elsewhere.',
                    'Missing physical/phase records remain unavailable; no time-proximity matching is used.',
                    'A capture must contain one boot and presenter lifetime; repeated serials are rejected.',
                ])
