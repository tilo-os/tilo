"""Synthetic input processing to next presented frame, in host-clock time.

This is not proof that every intermediate coalesced edit was visible, nor a
keypress-to-photon metric. Brackets bound sampling/float representation only;
no accuracy bound is claimed for the compositor's presentation timestamp.
"""
import math
import uuid
from native_delivery import rows, summarize_native_delivery
from presentation_feedback import summarize_presentation
from runtime_benchmark import distribution

_FIELDS = ('sent_host_lo_s', 'sent_host_hi_s',
           'dispatch_host_lo_s', 'dispatch_host_hi_s')


def _bounds(presented, lower, upper):
    # Widen each supplied float by one representable step and the final
    # subtraction once more. Never round a negative uncertainty bound to zero.
    low = math.nextafter(math.nextafter(presented, -math.inf)
                         - math.nextafter(upper, math.inf), -math.inf)
    high = math.nextafter(math.nextafter(presented, math.inf)
                          - math.nextafter(lower, -math.inf), math.inf)
    if high < 0:
        raise ValueError('Presentation precedes input host-clock bracket')
    bounds = [low*1000, high*1000]
    if not all(math.isfinite(value) for value in bounds):
        raise ValueError('Derived latency bounds overflow')
    return bounds


def summarize_input_latency(scenario, lines, frames):
    summarize_native_delivery(scenario, lines, frames)
    inputs = rows(lines, 'tilo-native-input ')
    available = [row.get('host_clock') == 'mach_absolute_seconds' for row in inputs]
    if not any(available):
        for row in inputs:
            legacy = 'host_clock' not in row and not any(k in row for k in _FIELDS)
            unsupported = (row.get('host_clock') == 'unavailable'
                           and all(k in row and row[k] is None for k in _FIELDS))
            if not (legacy or unsupported):
                raise ValueError('Malformed or unsupported host-clock evidence')
        return dict(available=False, latency_complete=False, performance_certified=False,
                    reason='Per-input host-clock brackets unavailable')
    if not all(available):
        raise ValueError('Partial per-input host-clock evidence')
    previous = None
    for row in inputs:
        if any(type(row.get(k)) not in (int, float) or not math.isfinite(row[k])
               or row[k] <= 0 for k in _FIELDS):
            raise ValueError('Invalid host-clock bracket')
        send_lo, send_hi, dispatch_lo, dispatch_hi = [row[k] for k in _FIELDS]
        if not send_lo <= send_hi <= dispatch_lo <= dispatch_hi:
            raise ValueError('Reversed host-clock bracket or delivery order')
        if previous and (send_lo < previous['sent_host_hi_s']
                         or dispatch_lo < previous['dispatch_host_hi_s']):
            raise ValueError('Host clock moved backwards')
        previous = row
    if rows(lines, 'tilo-native-clock-event '):
        raise ValueError('Clock lifecycle event invalidates continuous capture')
    mappings = rows(lines, 'tilo-native-frame ')
    expected = [row['cpu_frame_id'] for row in mappings]
    if ([identity for row in mappings for identity in row['input_ids']]
            != list(range(len(scenario['steps'])))
            or any(b <= a for a,b in zip(expected, expected[1:]))):
        raise ValueError('Native frame mappings are not ordered contiguous batches')
    presentation = summarize_presentation(lines, expected)
    if not presentation['feedback_complete']:
        raise ValueError('Incomplete positive presentation feedback for latency')
    times = {row['cpu_frame_id']:row['presented_time_s']
             for row in rows(lines, 'tilo-display-frame ')}
    frame_for = {identity:row for row in mappings for identity in row['input_ids']}
    samples, unresolved = [], []
    for row in inputs[scenario.get('warmup_steps', 0):]:
        mapping = frame_for[row['id']]
        presented = times[mapping['cpu_frame_id']]
        sent = _bounds(presented, row['sent_host_lo_s'], row['sent_host_hi_s'])
        dispatch = _bounds(presented, row['dispatch_host_lo_s'], row['dispatch_host_hi_s'])
        if sent[0] < 0 or dispatch[0] < 0:
            unresolved.append(row['id'])
        samples.append(dict(input_id=row['id'], cpu_frame_id=mapping['cpu_frame_id'],
                            frame_input_count=len(mapping['input_ids']),
                            sent_to_present_ms=sent, dispatch_to_present_ms=dispatch))
    complete = not unresolved
    report = dict(available=True, latency_complete=complete, performance_certified=False,
                  inputs=samples, unresolved_input_ids=unresolved,
                  coalesced_measured_inputs=sum(s['frame_input_count']>1 for s in samples),
                  clock='Mach absolute host seconds; not sleep-inclusive wall time',
                  metric='Input processing to next presented frame',
                  limitations=['Coalesced intermediate effects need not have appeared onscreen.',
                               'Bounds cover sampling and floating-point representation, not compositor accuracy.',
                               'Not physical device-to-photon latency or frame cadence qualification.'])
    for kind in ('sent', 'dispatch'):
        for index, end in enumerate(('lower', 'upper')):
            report[f'{kind}_to_present_{end}'] = (distribution(
                [s[f'{kind}_to_present_ms'][index] for s in samples], scale=1)
                if complete else None)
    return report


def summarize_physical_notes_latency(lines, scratch_page):
    """Trusted original kernel/libinput key time → exact changed Notes buffer → KMS flip.

    Never reads typed characters. Requires isolated one-key frames and complete
    positive standard Wayland feedback; absent/coalesced/unchanged state fails.
    This excludes peripheral scan before the trusted kernel/libinput event timestamp.
    """
    inputs = rows(lines, 'tilo-physical-input ')
    mappings = rows(lines, 'tilo-physical-frame ')
    deliveries = rows(lines, 'tilo-physical-delivery ')
    displayed = rows(lines, 'tilo-display-frame ')
    if not inputs or not scratch_page:
        raise ValueError('No physical Notes input or explicit scratch page')
    process_ids = {event.get('process_id') for event in inputs}
    if len(process_ids) != 1 or any(type(pid) is not int or pid <= 0 for pid in process_ids):
        raise ValueError('Physical input is not scoped to one actual Notes worker')
    process_id = next(iter(process_ids))
    sessions = {event.get('observer_session') for event in inputs}
    if (len(sessions) != 1 or any(not isinstance(value, str) or len(value) != 36 for value in sessions)):
        raise ValueError('Input lacks one sandbox-independent app worker lifetime identity')
    session = next(iter(sessions))
    try:
        parsed_session = uuid.UUID(session)
    except ValueError as error:
        raise ValueError('Malformed app worker lifetime UUID') from error
    if parsed_session.version != 4 or str(parsed_session) != session:
        raise ValueError('App worker lifetime identity is not a canonical random UUID')
    if any(frame.get('observer_session') != session or frame.get('process_id') != process_id
           for frame in mappings):
        raise ValueError('Frame belongs to a different Notes worker')
    def unique(values, key):
        out = {}
        for value in values:
            identity = value.get(key)
            if type(identity) is not int or identity < 0 or identity in out:
                raise ValueError('Missing or duplicate ' + key)
            out[identity] = value
        return out
    physical = unique(deliveries, 'wayland_serial')
    frames = unique(mappings, 'cpu_frame_id')
    feedback = unique([r for r in displayed if r.get('backend') == 'wayland'
                       and r.get('observer_session') == session and r.get('process_id') == process_id], 'cpu_frame_id')
    by_input = {}
    for frame in frames.values():
        identities = frame.get('input_ids')
        if not isinstance(identities, list) or len(identities) != 1:
            raise ValueError('Coalesced input does not prove every intermediate body was displayed')
        identity = identities[0]
        if type(identity) is not int:
            raise ValueError('Invalid physical input/frame mapping')
        by_input.setdefault(identity, []).append(frame)
    seen = set()
    samples = []
    previous_presented = previous_delivery = None
    surface_id = None
    for event in inputs:
        identity = event.get('id')
        if type(identity) is not int or identity in seen:
            raise ValueError('Missing or duplicate physical input identity')
        seen.add(identity)
        serial = event.get('wayland_serial')
        if event.get('kind') != 'key_pressed' or type(serial) is not int or serial not in physical:
            raise ValueError('Input lacks an exact trusted compositor key delivery')
        delivery = physical[serial]
        if (delivery.get('clock_id') != 1 or type(delivery.get('delivery_time_ns')) is not int
                or type(delivery.get('input_time_ns')) is not int):
            raise ValueError('Physical delivery lacks CLOCK_MONOTONIC evidence')
        if surface_id is None:
            surface_id = delivery.get('surface_id')
        if surface_id is None or delivery.get('surface_id') != surface_id:
            raise ValueError('Input changed its authoritative surface')
        if identity not in by_input:
            raise ValueError('Input has no drawn frame')
        candidates = by_input[identity]
        positive = [candidate for candidate in candidates
                    if feedback.get(candidate['cpu_frame_id'], {}).get('discarded') is False
                    and type(feedback[candidate['cpu_frame_id']].get('presented_time_ns')) is int]
        if not positive:
            raise ValueError('Exact drawn frame lacks positive hardware presentation feedback')
        frame = min(positive, key=lambda candidate:
                    feedback[candidate['cpu_frame_id']]['presented_time_ns'])
        # Earlier submitted buffers may be discarded. They must have exact
        # negative feedback and the same content-free drawn state, so a later
        # unrelated frame or a missing earlier presentation cannot qualify.
        for candidate in candidates:
            if candidate['cpu_frame_id'] >= frame['cpu_frame_id']:
                continue
            if (feedback.get(candidate['cpu_frame_id'], {}).get('discarded') is not True
                    or candidate.get('state') != frame.get('state')):
                raise ValueError('Earlier input draw is not a verified discarded identical body')
        before, state = event.get('before'), frame.get('state')
        for snap in (before, state):
            if not isinstance(snap, dict) or snap.get('app') != 'so.tilo.notes':
                raise ValueError('Notes content-free state missing')
            if any(snap.get(k) != scratch_page for k in
                   ('selected_page', 'main_page', 'editor_binding_page')):
                raise ValueError('Input is not bound to the explicit scratch page')
            if snap.get('editor_focused') is not True or snap.get('body_hash') != snap.get('rendered_body_hash'):
                raise ValueError('Canonical body and drawn editor disagree')
            if type(snap.get('body_chars')) is not int or type(snap.get('body_bytes')) is not int:
                raise ValueError('Body count evidence missing')
            if (snap.get('hash_algorithm') != 'blake3' or not isinstance(snap.get('body_hash'), str)
                    or len(snap['body_hash']) != 64
                    or any(c not in '0123456789abcdef' for c in snap['body_hash'])):
                raise ValueError('Body hash evidence missing')
        if (state['body_hash'] == before['body_hash'] or state['body_chars'] != before['body_chars'] + 1
                or state['body_bytes'] <= before['body_bytes']
                or state.get('caret', {}).get('byte_offset') != state['body_bytes']):
            raise ValueError('This frame does not prove one inserted scratch character')
        effect = event.get('after')
        if (not isinstance(effect, dict)
                or effect.get('rendered_body_hash') != state['body_hash']
                or effect.get('rendered_body_chars') != state['body_chars']
                or any(effect.get(k) != scratch_page for k in
                       ('selected_page', 'main_page', 'editor_binding_page'))):
            raise ValueError('Drawn body is not the effect of this dispatched input')
        presented = feedback.get(frame['cpu_frame_id'])
        if (not presented or presented.get('discarded') is not False
                or presented.get('clock_id') != 1 or type(presented.get('flags')) is not int
                or presented['flags'] & 7 != 7
                or type(presented.get('presented_time_ns')) is not int):
            raise ValueError('Exact drawn frame lacks positive hardware presentation feedback')
        start, delivered, end = delivery['input_time_ns'], delivery['delivery_time_ns'], presented['presented_time_ns']
        lo, hi = event.get('dispatch_lo_s'), event.get('dispatch_hi_s')
        if (event.get('clock') != 'clock_monotonic_seconds'
                or type(lo) not in (int, float) or type(hi) not in (int, float)
                or not math.isfinite(lo) or not math.isfinite(hi)
                or not 0 <= start <= delivered <= lo*1e9 <= hi*1e9 <= end):
            raise ValueError('Missing or invalid kernel/delivery/client/presentation clock ordering')
        if end < start or start < 0:
            raise ValueError('Presentation precedes physical delivery')
        if previous_presented is not None and (start <= previous_presented or end <= previous_presented):
            raise ValueError('Typing was not isolated after the preceding presented body')
        if previous_delivery is not None and start <= previous_delivery:
            raise ValueError('Physical delivery clock moved backwards')
        previous_presented, previous_delivery = end, start
        samples.append({'input_id': identity, 'cpu_frame_id': frame['cpu_frame_id'],
                        'wayland_serial': serial, 'latency_ms': (end - start) / 1e6,
                        'delivery_to_present_ms': (end - delivered) / 1e6})
    if set(by_input) != seen:
        raise ValueError('Extra or missing physical input mappings')
    measured = distribution([sample['latency_ms'] for sample in samples], scale=1)
    return {'latency_complete': True, 'metric': 'Original trusted kernel/libinput key time to exact changed Notes body KMS presentation',
            'clock': 'CLOCK_MONOTONIC', 'samples': samples, 'latency': measured,
            'limitations': ['Excludes peripheral scan before the trusted kernel/libinput timestamp.',
                            'Requires isolated character presses, no coalesced or IME-only samples.',
                            'Includes opt-in content-free state and timing diagnostics; their overhead must be checked before normal-mode performance certification.',
                            'Hardware presentation timestamps do not measure panel pixel response.']}
