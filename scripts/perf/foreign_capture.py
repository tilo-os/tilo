"""Exact capture-token/painted-generation/presenter joins; never nearest times."""
import json
from render_boundaries import _interval
from runtime_benchmark import distribution

PREFIXES = ('tilo-foreign-capture ', 'tilo-compositor-feedback ', 'tilo-present-pipe ')
MAX_RECORDS = 10000


def records(lines, prefix):
    result = []
    for line in lines:
        if not line.startswith(prefix):
            continue
        if len(result) >= MAX_RECORDS or len(line) > 4096:
            raise ValueError('Capture timing records exceed their bound')
        row = json.loads(line[len(prefix):])
        if not isinstance(row, dict):
            raise ValueError('Invalid capture timing record')
        result.append(row)
    return result


def number(row, key):
    value = row.get(key)
    if type(value) is not int or not 0 <= value <= 2**64 - 1:
        raise ValueError('Missing or invalid capture timing integer: ' + key)
    return value


def indexed(rows, keys):
    result = {}
    for row in rows:
        identity = tuple(number(row, key) for key in keys)
        if identity in result:
            raise ValueError('Duplicate capture timing identity')
        result[identity] = row
    return result


def summarize_foreign_capture(lines, compositor_frames, samples, displays, deliveries, events, presents, required=False):
    groups = [records(lines, prefix) for prefix in PREFIXES]
    if not any(groups):
        if required:
            raise ValueError('Missing required foreign capture timing evidence')
        return dict(available=False, complete=False, performance_certified=False)
    imported = indexed([row for row in groups[0] if row.get('event') == 'imported'], ('surface_id', 'capture_token'))
    activated = indexed([row for row in groups[0] if row.get('event') == 'activated'], ('surface_id', 'capture_seq'))
    submitted = indexed([row for row in groups[1] if row.get('event') == 'submitted'], ('pipe_serial', 'surface_id', 'capture_seq'))
    completed = [row for row in groups[1] if row.get('event') == 'completed']
    indexed(completed, ('pipe_serial', 'surface_id', 'capture_seq'))
    pipes = indexed([row for row in groups[2] if row.get('event') == 'retired'], ('pipe_serial',))
    phases = {row['cpu_frame_id']: row for row in compositor_frames}
    frames = []
    for sample in samples:
        cpu = sample['cpu_frame_id']
        phase = phases.get(cpu, {})
        if not phase.get('compositor_match_available'):
            raise ValueError('Capture timing lacks exact compositor physical ownership')
        serial = phase['serial']
        display = displays[cpu]
        event = events[sample['input_id']]
        delivery = deliveries[event['wayland_serial']]
        surface = number(delivery, 'surface_id')
        matches = [row for row in completed if row.get('pipe_serial') == serial
                   and row.get('surface_id') == surface
                   and row.get('physical_presented_time_ns') == display['presented_time_ns']
                   and row.get('physical_sequence') == display['sequence']]
        if len(matches) != 1:
            raise ValueError('Missing or ambiguous composed generation for physical input')
        finish = matches[0]
        number(finish, 'physical_presented_time_ns')
        number(finish, 'physical_sequence')
        seq = number(finish, 'capture_seq')
        submit = submitted.get((serial, surface, seq))
        active = activated.get((surface, seq))
        pipe = pipes.get((serial,))
        if submit is None or active is None or pipe is None:
            raise ValueError('Incomplete capture generation/presenter join')
        capture = imported.get((surface, number(active, 'capture_token')))
        if capture is None:
            raise ValueError('Missing exact activated capture token')
        if any(type(row.get('clock_id')) is not int or row['clock_id'] != 1 for row in (finish, submit, active, pipe, capture)):
            raise ValueError('Capture timing requires the monotonic clock')
        if (finish.get('omitted_pixels') != 0 or submit.get('omitted_pixels') != 0
                or capture.get('accepted') is not True or active.get('accepted') is not True
                or capture.get('shared_gpu') is not True):
            raise ValueError('Capture timing cannot certify omitted or failed GPU ownership')
        keys = ('prepare_ns', 'enqueue_ns', 'worker_start_ns', 'fence_start_ns', 'fence_end_ns',
                'native_import_end_ns', 'copy_submit_ns', 'copy_submit_return_ns', 'copy_end_ns',
                'staged_ns', 'barrier_ns')
        points = [number(capture, key) for key in keys]
        activation = number(active, 'activated_ns')
        composition = number(submit, 'composed_time_ns')
        if finish.get('composed_time_ns') != composition:
            raise ValueError('Composed generation changed across presenter bookkeeping')
        points += [activation, composition, number(pipe, 'enqueue_ns'),
                   number(pipe, 'render_start_ns'), number(pipe, 'render_end_ns')]
        if any(left > right for left, right in zip(points, points[1:])):
            raise ValueError('Capture timing causal order is reversed')
        barrier_return = number(capture, 'barrier_return_ns')
        if barrier_return < number(capture, 'barrier_ns'):
            raise ValueError('Invalid capture barrier clock bracket')
        names = ('capture_prepare', 'capture_queue', 'capture_worker_prepare', 'implicit_fence_wait',
                 'native_import', 'owned_copy_prepare', 'owned_copy_submit', 'owned_copy_wait',
                 'capture_stage', 'capture_barrier_prepare', 'barrier_to_activation',
                 'activation_to_composition', 'composition_to_enqueue', 'presenter_queue', 'presenter_render')
        intervals = {name: _interval([left / 1e9] * 2, [right / 1e9] * 2)
                     for name, left, right in zip(names, points, points[1:])}
        intervals['barrier_to_activation'] = _interval(
            [number(capture, 'barrier_ns') / 1e9, min(barrier_return, activation) / 1e9],
            [activation / 1e9] * 2)
        present = presents[cpu]
        intervals['present_return_to_capture_prepare'] = _interval(
            [present['after_host_lo_s'], present['after_host_hi_s']], [points[0] / 1e9] * 2)
        breakdown = dict(available=False)
        if capture.get('copy_encoder_finish_return_ns') is not None:
            encoded = number(capture, 'copy_encoder_finish_return_ns')
            begin, end = number(capture, 'copy_submit_ns'), number(capture, 'copy_submit_return_ns')
            if not begin <= encoded <= end:
                raise ValueError('Command encoder finish is outside its submission bracket')
            breakdown = dict(available=True, encoder_finish_ms=(encoded - begin) / 1e6,
                             queue_submit_ms=(end - encoded) / 1e6)
        if 'fence_ready' in capture and capture['fence_ready'] is not True:
            raise ValueError('Producer fence did not prove write completion')
        frames.append(dict(input_id=sample['input_id'], cpu_frame_id=cpu, surface_id=surface,
                           capture_token=active['capture_token'], capture_seq=seq, pipe_serial=serial,
                           intervals_ms=intervals, copy_submit_breakdown=breakdown))
    return dict(available=bool(frames), complete=bool(samples) and len(frames) == len(samples),
                performance_certified=False, frames=frames,
                stages={name: {bound: distribution([frame['intervals_ms'][name][index] for frame in frames], scale=1)
                               for index, bound in enumerate(('lower', 'upper'))}
                        for name in frames[0]['intervals_ms']} if frames else {})
