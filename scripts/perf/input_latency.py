"""Synthetic input processing to next presented frame, in host-clock time.

This is not proof that every intermediate coalesced edit was visible, nor a
keypress-to-photon metric. Brackets bound sampling/float representation only;
no accuracy bound is claimed for the compositor's presentation timestamp.
"""
import math
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
    inputs = rows(lines, 'qos-native-input ')
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
    if rows(lines, 'qos-native-clock-event '):
        raise ValueError('Clock lifecycle event invalidates continuous capture')
    mappings = rows(lines, 'qos-native-frame ')
    expected = [row['cpu_frame_id'] for row in mappings]
    if ([identity for row in mappings for identity in row['input_ids']]
            != list(range(len(scenario['steps'])))
            or any(b <= a for a,b in zip(expected, expected[1:]))):
        raise ValueError('Native frame mappings are not ordered contiguous batches')
    presentation = summarize_presentation(lines, expected)
    if not presentation['feedback_complete']:
        raise ValueError('Incomplete positive presentation feedback for latency')
    times = {row['cpu_frame_id']:row['presented_time_s']
             for row in rows(lines, 'qos-display-frame ')}
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
