"""Optional native Vello evidence; exact recording ownership, never a host clock."""
import math
import re

from native_delivery import rows
from runtime_benchmark import distribution

PREFIX = 'tilo-native-gpu-stages '
KINDS = ('passes', 'dispatches')
MAX_ROWS = 10000
MAX_STAGES = 64


def uint(value, limit=(1 << 64) - 1):
    if type(value) is not int or not 0 <= value <= limit:
        raise ValueError('Invalid native GPU unsigned metadata')
    return value


def sample_metrics(kind, sample):
    if not isinstance(sample, dict):
        raise ValueError('Missing native GPU sample metrics')
    if kind == 'passes':
        durations = {key: sample.get(key) for key in ('span_ns', 'summed_pass_ns')}
        if any(type(value) not in (int, float) or not math.isfinite(value) or value < 0
               for value in durations.values()):
            raise ValueError('Invalid native GPU device duration')
        return dict(pass_count=uint(sample.get('pass_count')), **durations)
    stages = sample.get('stages')
    count = uint(sample.get('stage_count'), MAX_STAGES)
    if not isinstance(stages, list) or len(stages) != count:
        raise ValueError('Partial native GPU dispatch stages')
    result, seen = [], set()
    for stage in stages:
        if not isinstance(stage, dict):
            raise ValueError('Invalid native GPU dispatch stage')
        identity = uint(stage.get('stage_id'), 65535)
        name, duration = stage.get('stage'), stage.get('duration_ns')
        if (identity in seen or not isinstance(name, str)
                or not re.fullmatch(r'[a-z_][a-z0-9_]{0,47}', name)
                or not isinstance(duration, str)
                or not re.fullmatch(r'0|[1-9][0-9]{0,38}', duration)):
            raise ValueError('Invalid native GPU dispatch stage')
        seen.add(identity)
        result.append(dict(stage_id=identity, stage=name,
                           dispatch_count=uint(stage.get('dispatch_count')),
                           duration_ns=uint(int(duration), (1 << 128) - 1)))
    return dict(stage_count=count, stages=result)


def kind_evidence(records, kind):
    relevant = [row for row in records if row.get('kind') == kind]
    attempts = [row for row in relevant if row.get('event') == 'attempt']
    completed = [row for row in relevant if row.get('event') == 'completed']
    if len(attempts) > 1:
        raise ValueError('Ambiguous native GPU recording ownership')
    attempt = attempts[0] if attempts else None
    result = dict(status='missing_current', available=False, delayed=False)
    if not relevant:
        return result
    if any(row.get('stale_sample') is True for row in relevant):
        result['status'] = 'stale_sample'
        return result
    if not completed:
        if any(row.get('event') == 'owner_evicted' for row in relevant):
            result['status'] = 'owner_evicted'
        elif attempt is not None and attempt['supported'] is False:
            result['status'] = 'unsupported'
        return result
    if attempt is None or attempt.get('expected_current_submission_id') is None:
        result['status'] = 'unmatched_recording'
        return result
    explicit_range = 'recording_range_first' in attempt or 'recording_range_last' in attempt
    if explicit_range:
        first, last = (uint(attempt.get(key)) for key in
                       ('recording_range_first', 'recording_range_last'))
        if first == 0 or first > last or last != attempt['expected_current_submission_id']:
            raise ValueError('Invalid native GPU recording range')
    else:
        first = last = uint(attempt['expected_current_submission_id'])
    samples, seen = [], set()
    for completion in completed:
        submission = uint(completion.get('sample_submission_id'))
        if submission in seen:
            raise ValueError('Ambiguous native GPU recording ownership')
        seen.add(submission)
        if (completion.get('matched_current') is not True
                or completion.get('missing_current') is not False
                or completion['supported'] is not True
                or completion.get('expected_current_submission_id') != submission
                or not first <= submission <= last
                or (explicit_range and (completion.get('recording_range_first'),
                                        completion.get('recording_range_last')) != (first, last))):
            result['status'] = 'unmatched_recording'
            return result
        delayed = completion.get('latest_submission_id') != submission
        if completion.get('delayed') is not delayed:
            raise ValueError('Inconsistent native GPU delayed-recording evidence')
        samples.append(dict(submission_id=submission, delayed=delayed,
                            sample=sample_metrics(kind, completion.get('sample'))))
    samples.sort(key=lambda sample: sample['submission_id'])
    result.update(status='matched_current', available=True,
                  delayed=any(sample['delayed'] for sample in samples), samples=samples,
                  recording_range=dict(first=first, last=last),
                  measured_recordings=len(samples), allocated_recordings=last - first + 1,
                  all_recordings_measured=len(samples) == last - first + 1)
    # Preserve the single-recording interface. Multiple samples stay distinct;
    # device spans overlap and must never be fabricated into one whole frame.
    if len(samples) == 1:
        result.update(submission_id=samples[0]['submission_id'], sample=samples[0]['sample'])
    return result


def summarize_native_gpu(lines, frames, producer):
    records = rows(lines, PREFIX)
    if len(records) > MAX_ROWS:
        raise ValueError('Native GPU evidence exceeds record bound')
    expected = {(frame['cpu_frame_id'], frame['worker_id'], frame['worker_frame_ix'])
                for frame in frames}
    owned, excluded, stale = {}, 0, 0
    excluded_lifetime, excluded_frame = 0, 0
    for row in records:
        if not isinstance(row, dict):
            raise ValueError('Invalid native GPU evidence record')
        if (row.get('process_id'), row.get('observer_session')) != producer:
            excluded += 1
            excluded_lifetime += 1
            continue
        if row.get('stale_sample') is True and row.get('cpu_frame_id') is None:
            stale += 1
            continue  # The recorder explicitly cannot name an owner; never guess one.
        identity = tuple(row.get(key) for key in ('cpu_frame_id', 'worker_id', 'worker_frame_ix'))
        if any(value is not None and type(value) is not int for value in identity):
            raise ValueError('Invalid native GPU frame identity')
        if identity not in expected:
            excluded += 1
            excluded_frame += 1
            continue
        if (row.get('kind') not in KINDS
                or row.get('event') not in ('attempt', 'completed', 'owner_evicted')
                or any(type(row.get(key)) is not bool for key in
                       ('supported', 'pending', 'matched_current', 'missing_current',
                        'stale_sample', 'delayed'))):
            raise ValueError('Invalid native GPU evidence status')
        for value in identity:
            uint(value)
        for key in ('expected_current_submission_id', 'latest_submission_id', 'sample_submission_id',
                    'recording_range_first', 'recording_range_last'):
            if row.get(key) is not None:
                uint(row[key])
        owned.setdefault(identity, []).append(row)
    evidence = []
    for frame in frames:
        identity = tuple(frame[key] for key in ('cpu_frame_id', 'worker_id', 'worker_frame_ix'))
        kinds = {kind: kind_evidence(owned.get(identity, []), kind) for kind in KINDS}
        evidence.append(dict(input_id=frame['input_id'], cpu_frame_id=identity[0],
                             worker_id=identity[1], worker_frame_ix=identity[2],
                             complete=all(value['available'] for value in kinds.values()),
                             kinds=kinds))
    costs = {}
    for key in ('span_ns', 'summed_pass_ns'):
        values = [sample['sample'][key] for frame in evidence
                  if frame['kinds']['passes']['available']
                  for sample in frame['kinds']['passes']['samples']]
        if values:
            costs[key] = distribution(values)
    stages = {}
    for frame in evidence:
        dispatch = frame['kinds']['dispatches']
        if dispatch['available']:
            for sample in dispatch['samples']:
                for stage in sample['sample']['stages']:
                    key = stage['stage_id'], stage['stage']
                    stages.setdefault(key, []).append(stage['duration_ns'])
    return dict(available=any(value['available'] for frame in evidence
                              for value in frame['kinds'].values()),
                complete=all(frame['complete'] for frame in evidence),
                performance_certified=False, clock='device_relative_duration_ns',
                frames=evidence, pass_costs=costs,
                dispatch_costs=[dict(stage_id=key[0], stage=key[1], duration=distribution(values))
                                for key, values in sorted(stages.items())],
                coverage=dict(records=len(records), excluded_identity_records=excluded,
                              excluded_other_lifetime_records=excluded_lifetime,
                              excluded_other_frame_records=excluded_frame,
                              unassigned_stale_samples=stale),
                limitations=['Optional GPU evidence never certifies input or physical latency.',
                             'Pass and dispatch spans overlap; do not add them.',
                             'Device durations cannot be joined to host timestamps.',
                             'Allocated recording IDs do not imply timestamp queries; pending slots leave gaps.',
                             'Individual samples are not whole-frame GPU cost.',
                             'Vello samples exclude later blits and the whole producer fence.'])
