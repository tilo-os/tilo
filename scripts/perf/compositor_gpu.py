"""Bounded compositor GPU recording ownership; output time is nested, not additive."""

MAX_RECORDS = 10000


def integer(value):
    if not isinstance(value, str) or not value.isascii() or not value.isdecimal():
        raise ValueError("Invalid compositor GPU integer")
    return int(value)


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


def summarize_compositor_gpu(lines):
    """Join envelopes to main's retired-frame records, not a proximity estimate.

    Coverage describes encoder work only, not physical display completion.
    """
    lines = list(lines)
    submissions = gpu_submission_records(lines)
    frames = {}
    for line in lines:
        _, found, suffix = line.partition('FRAME-LIFECYCLE ')
        if not found:
            continue
        if len(suffix) > 4096:
            raise ValueError('Frame lifecycle record exceeds its bound')
        row = {}
        for token in suffix.split():
            key, separator, value = token.partition('=')
            if not separator or key in row:
                raise ValueError('Invalid frame lifecycle field')
            row[key] = value
        if row.get('event') != 'render-retired':
            continue
        serial = integer(row.get('serial'))
        revision = integer(row.get('revision'))
        if not serial or serial in frames or len(frames) >= MAX_RECORDS:
            raise ValueError('Invalid or duplicate retired frame identity')
        if row.get('phase_timing') not in ('true', 'false'):
            raise ValueError('Missing frame phase coverage')
        gpu = submissions.get(serial, dict(status='missing', available=False))
        if 'revision' in gpu and gpu['revision'] != revision:
            raise ValueError('GPU recording belongs to another frame revision')
        if gpu['available'] and row['phase_timing'] != 'true':
            raise ValueError('GPU coverage has no completed frame phases')
        frames[serial] = dict(revision=revision, submission_gpu=gpu)
    if not submissions.keys() <= frames.keys():
        raise ValueError('GPU submission lacks a retired frame')
    return dict(frames=frames, performance_certified=False,
                limitations=['Device-clock encoder intervals exclude pre-execution queue delay and query resolve.',
                             'Post-render output time is nested inside recording time; never add them.',
                             'A retired frame is not evidence of physical display completion.'])
