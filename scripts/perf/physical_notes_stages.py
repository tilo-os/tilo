"""Join Linux client CPU stages to verified physical native presentations.

This attributes elapsed CPU/queue time, including scheduler preemption. It
does not certify performance. Optional native Vello samples are device-relative
durations attributed only by their recorded producer, frame and submission;
they are not the whole producer fence or part of the host-clock intervals.
"""
import argparse
import json
import math
import os
from pathlib import Path

from foreign_capture import summarize_foreign_capture
from compositor_phases import capture_lines, summarize_compositor_phases
from input_latency import (summarize_physical_notes_latency,
                           summarize_physical_settings_latency,
                           summarize_physical_settings_hover_latency, _pointer_identity)
from main_stages import POINTS, STAGES
from native_delivery import rows
from native_gpu_evidence import summarize_native_gpu
from render_boundaries import _interval, _ordered
from runtime_benchmark import distribution


def bracket(row, prefix):
    values = [row.get(prefix + '_host_' + end + '_s') for end in ('lo', 'hi')]
    if (row.get('host_clock') != 'clock_monotonic_seconds'
            or any(type(value) not in (int, float) or not math.isfinite(value)
                   or value < 0 for value in values)
            or values[0] > values[1]):
        raise ValueError('Invalid Linux render clock bracket')
    return values


def summarize_physical_notes_stages(lines, scratch_page, compositor_lines=None, require_capture=False):
    return _summarize_stages(lines, summarize_physical_notes_latency(lines, scratch_page),
                             compositor_lines, require_capture)


def summarize_physical_settings_stages(lines, expected_pane, compositor_lines=None,
                                      require_capture=False, *, hover=False):
    """Exact changed native Settings pane or hover; synthetic origins never qualify."""
    summarize = (summarize_physical_settings_hover_latency if hover
                 else summarize_physical_settings_latency)
    return _summarize_stages(lines, summarize(lines, expected_pane),
                             compositor_lines, require_capture, pointer=hover)


def _summarize_stages(lines, latency, compositor_lines, require_capture, pointer=False):
    inputs = rows(lines, 'tilo-physical-input ')
    producer = inputs[0]['process_id'], inputs[0]['observer_session']

    def owned(prefix):
        result = {}
        for row in rows(lines, prefix):
            if (row.get('process_id'), row.get('observer_session')) != producer:
                continue
            cpu = row.get('cpu_frame_id')
            if cpu is None:
                continue  # Auxiliary frames are not the changed native state.
            if type(cpu) is not int or cpu < 0 or cpu in result:
                raise ValueError('Invalid or duplicate Linux stage frame identity')
            result[cpu] = row
        return result

    sends = owned('tilo-native-send ')
    workers = owned('tilo-native-worker ')
    presents = owned('tilo-native-present-call ')
    mains = owned('tilo-native-main-stages ')
    feedback = owned('tilo-display-frame ')
    events = {row['id']: row for row in inputs}
    marker = 'tilo-physical-pointer-delivery ' if pointer else 'tilo-physical-delivery '
    deliveries = {(_pointer_identity(row, delivery=True) if pointer else row['wayland_serial']): row
                  for row in rows(lines, marker)}
    frames = []
    seen_workers = set()
    for sample in latency['samples']:
        cpu = sample['cpu_frame_id']
        if any(cpu not in group for group in (sends, workers, presents, feedback)):
            raise ValueError('Missing render stages for a verified native frame')
        send, worker, present, display = [group[cpu] for group in
                                           (sends, workers, presents, feedback)]
        if send.get('send_ok') is not True or worker.get('outcome') != 'Presented':
            raise ValueError('Native frame was not successfully sent and presented')
        for row in (worker, present, display):
            if any(type(row.get(key)) is not int or row[key] < 0
                   for key in ('worker_id', 'worker_frame_ix')):
                raise ValueError('Missing Linux render worker identity')
        if any(worker[key] != row[key] for row in (present, display)
               for key in ('worker_id', 'worker_frame_ix')):
            raise ValueError('Render worker does not match physical presentation')
        worker_key = worker['worker_id'], worker['worker_frame_ix']
        if worker_key in seen_workers:
            raise ValueError('Render worker frame was reused for different inputs')
        seen_workers.add(worker_key)
        before, after = [bracket(send, point) for point in ('before', 'after')]
        receive, start, end = [bracket(worker, point) for point in
                               ('receive', 'render_start', 'render_end')]
        present_before, present_after = [bracket(present, point)
                                        for point in ('before', 'after')]
        event = events[sample['input_id']]
        entry = [event['dispatch_lo_s'], event['dispatch_hi_s']]
        shown = [display['presented_time_ns'] / 1e9] * 2
        key = _pointer_identity(event) if pointer else sample['wayland_serial']
        delivery = deliveries[key]
        kernel = [delivery['input_time_ns'] / 1e9] * 2
        delivered = [delivery['delivery_time_ns'] / 1e9] * 2
        _ordered(kernel, delivered, entry, before, receive, start,
                 present_before, present_after, end)
        _ordered(before, after)
        if shown[1] < present_before[0]:
            raise ValueError('KMS presentation precedes the client present call')
        pairs = dict(kernel_to_delivery=(kernel, delivered),
                     delivery_to_client=(delivered, entry),
                     client_entry_to_send=(entry, before),
                     send_call=(before, after),
                     send_start_to_receive=(before, receive),
                     receive_to_render_start=(receive, start),
                     worker_render_cpu=(start, end),
                     render_start_to_present_call=(start, present_before),
                     present_call_cpu=(present_before, present_after),
                     present_return_to_display=(present_after, shown))
        native_fields = [f'{point}_host_{bound}_s'
                         for point in ('native_before', 'native_after') for bound in ('lo', 'hi')]
        native_available = any(present.get(field) is not None for field in native_fields)
        if native_available:
            native_before, native_after = [bracket(present, point)
                                           for point in ('native_before', 'native_after')]
            _ordered(present_before, native_before, native_after, present_after)
            pairs.update(present_wrapper_before_native=(present_before, native_before),
                         native_present_cpu=(native_before, native_after),
                         present_wrapper_after_native=(native_after, present_after))
        if mains:
            if cpu not in mains:
                raise ValueError('Partial native main-thread stage evidence')
            points = [bracket(mains[cpu], point) for point in POINTS]
            _ordered(entry, *points, before)
            pairs.update(zip(STAGES, zip(points, points[1:])))
            pairs['pre_send_gap'] = points[-1], before
        frames.append(dict(input_id=sample['input_id'], cpu_frame_id=cpu,
                           worker_id=worker_key[0], worker_frame_ix=worker_key[1],
                           native_present_samples_available=native_available,
                           intervals_ms={key: _interval(*pair)
                                         for key, pair in pairs.items()}))
    stages = {key: {bound: distribution(
        [frame['intervals_ms'][key][index] for frame in frames], scale=1)
        for index, bound in enumerate(('lower', 'upper'))}
        for key in set.intersection(*(set(frame['intervals_ms']) for frame in frames))}
    phase_lines = lines if compositor_lines is None else compositor_lines
    compositor = summarize_compositor_phases(phase_lines, latency['samples'], feedback)
    capture = summarize_foreign_capture(phase_lines, compositor['frames'], latency['samples'],
                                        feedback, deliveries, events, presents, require_capture)
    return dict(available=True, performance_certified=False, metric=latency['metric'],
                main_stages_available=bool(mains),
                clock='CLOCK_MONOTONIC', process_id=producer[0],
                observer_session=producer[1], frames=frames, stages=stages,
                latency=latency['latency'],
                compositor=compositor, foreign_capture=capture,
                native_gpu=summarize_native_gpu(lines, frames, producer),
                limitations=['CPU stages include scheduling and logging overhead.',
                             'CPU render duration is not GPU execution duration.',
                             'KMS timestamps do not measure panel pixel response.'])


def require_gpu_stages(report):
    """GPU execution attribution requires complete exact-owned samples, never CPU fence time."""
    compositor = report['compositor']
    if (not report['native_gpu']['complete'] or not compositor['complete']
            or not report['foreign_capture']['complete']
            or any(not frame.get('submission_gpu', {}).get('available')
                   for frame in compositor['frames'])):
        raise ValueError('Incomplete exact native/compositor GPU or capture coverage')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--capture', type=Path, required=True,
                        help='directory produced by device_notes_latency.py')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--compositor-log', type=Path,
                        help='bounded lifecycle capture from the same boot/presenter; defaults to timeline.log')
    parser.add_argument('--require-foreign-capture', action='store_true',
                        help='fail unless every physical sample has exact capture/paint/presenter stages')
    settings = parser.add_mutually_exclusive_group()
    settings.add_argument('--settings-pane', help='expected pane changed by an ordinary native key')
    settings.add_argument('--settings-hover', help='expected sidebar pane hovered by an ordinary motion')
    parser.add_argument('--require-gpu-stages', action='store_true',
                        help='fail unless exact native GPU and compositor GPU coverage are complete')
    options = parser.parse_args()
    lines = capture_lines(options.capture / 'timeline.log')
    compositor = capture_lines(options.compositor_log) if options.compositor_log else None
    if options.settings_pane or options.settings_hover:
        report = summarize_physical_settings_stages(
            lines, options.settings_pane or options.settings_hover, compositor,
            options.require_foreign_capture, hover=bool(options.settings_hover))
    else:
        report = summarize_physical_notes_stages(
            lines, json.loads((options.capture / 'capture.json').read_text())['scratch_page'],
            compositor, options.require_foreign_capture)
    if options.require_gpu_stages:
        require_gpu_stages(report)
    descriptor = os.open(options.out, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'w') as output:
        json.dump(report, output, indent=2)
        output.write('\n')
    print('Verified Linux native frames:', len(report['frames']))
    gpu = report['native_gpu']
    print('Exact native GPU evidence:', sum(frame['complete'] for frame in gpu['frames']),
          '/', len(gpu['frames']), 'frames complete; optional device durations only')


if __name__ == '__main__':
    main()
