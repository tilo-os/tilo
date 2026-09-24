"""Validate native presentation feedback without promoting missing data to success."""
import math
from native_delivery import rows
from runtime_benchmark import distribution


def summarize_presentation(lines, expected_cpu_ids):
    expected = set(expected_cpu_ids)
    if len(expected) != len(expected_cpu_ids) or any(type(i) is not int or i < 0 for i in expected):
        raise ValueError('Invalid expected CPU identities')
    requests, callbacks, cpu_requests = {}, {}, {}
    for prefix, destination in [('qos-display-request ', requests), ('qos-display-frame ', callbacks)]:
        for row in rows(lines, prefix):
            if any(type(row.get(k)) is not int or row[k] < 0 for k in ['worker_id','worker_frame_ix']):
                raise ValueError('Invalid display worker identity')
            cpu = row.get('cpu_frame_id')
            if cpu is not None and (type(cpu) is not int or cpu < 0):
                raise ValueError('Invalid display CPU identity')
            key = row['worker_id'], row['worker_frame_ix']
            if key in destination:
                raise ValueError('Duplicate display request or callback')
            destination[key] = row
            if prefix == 'qos-display-request ' and cpu is not None:
                if cpu in cpu_requests:
                    raise ValueError('CPU frame registered more than once')
                cpu_requests[cpu] = key
    if not expected or not expected <= cpu_requests.keys():
        raise ValueError('Missing display requests for expected CPU frames')
    seen_drawables = set()
    for key, row in callbacks.items():
        if key not in requests or row['cpu_frame_id'] != requests[key]['cpu_frame_id']:
            raise ValueError('Display callback has no matching request')
        drawable = row.get('drawable_id')
        if row.get('backend') != 'metal' or type(drawable) is not int or drawable < 0:
            raise ValueError('Invalid presentation backend or drawable identity')
        drawable_key = key[0], drawable
        if drawable_key in seen_drawables:
            raise ValueError('Drawable identity reused within a render worker')
        seen_drawables.add(drawable_key)
        value = row.get('presented_time_s')
        if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
            raise ValueError('Invalid presentation timestamp')
    keys = sorted(cpu_requests[cpu] for cpu in expected)
    if len({key[0] for key in keys}) != 1:
        raise ValueError('Presentation intervals cannot span different render workers')
    missing = sorted(requests[key]['cpu_frame_id'] for key in keys if key not in callbacks)
    zero = sorted(callbacks[key]['cpu_frame_id'] for key in keys
                  if key in callbacks and callbacks[key]['presented_time_s'] == 0)
    positive = [callbacks[key] for key in keys
                if key in callbacks and callbacks[key]['presented_time_s'] > 0]
    nonmonotonic = [b['cpu_frame_id'] for a,b in zip(positive, positive[1:])
                    if b['presented_time_s'] <= a['presented_time_s']]
    complete = not (missing or zero or nonmonotonic)
    intervals = [b['presented_time_s']-a['presented_time_s'] for a,b in zip(positive,positive[1:])]
    return {
        'performance_certified': False,
        'feedback_complete': complete,
        'expected_cpu_frames': len(expected),
        'positive_presentations': len(positive),
        'missing_callback_cpu_ids': missing,
        'zero_timestamp_cpu_ids': zero,
        'nonmonotonic_timestamp_cpu_ids': nonmonotonic,
        'presentation_intervals': distribution(intervals, scale=1000) if complete and intervals else None,
        'clock': 'Metal presentedTime host seconds; no input-clock correlation assumed',
        'limitations': ['Complete feedback alone does not validate pixels or workload coverage.',
                       'Missing callbacks remain unknown; zero timestamps are not positive presentation evidence.'],
    }
