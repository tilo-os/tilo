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


def summarize_compositor_phases(lines, samples, feedback):
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
        record = dict(serial=serial, revision=revision, phase_samples_available=phase_complete,
                      costs_us=costs, byte_counts=byte_counts, physical_outcome=proof)
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
                    'Completion wait includes GPU waiting or readback mapping and is not GPU execution duration.',
                    'No compositor readback bytes does not prove absence of copies elsewhere.',
                    'Missing physical/phase records remain unavailable; no time-proximity matching is used.',
                    'A capture must contain one boot and presenter lifetime; repeated serials are rejected.',
                ])
