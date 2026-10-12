"""Join Linux client CPU stages to verified physical Notes presentations.

This attributes elapsed CPU/queue time, including scheduler preemption. It
does not measure GPU completion or certify performance.
"""
import argparse
import json
import math
import os
from pathlib import Path

from input_latency import summarize_physical_notes_latency
from main_stages import POINTS, STAGES
from native_delivery import rows
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


def summarize_physical_notes_stages(lines, scratch_page):
    latency = summarize_physical_notes_latency(lines, scratch_page)
    inputs = rows(lines, 'tilo-physical-input ')
    producer = inputs[0]['process_id'], inputs[0]['observer_session']

    def owned(prefix):
        result = {}
        for row in rows(lines, prefix):
            if (row.get('process_id'), row.get('observer_session')) != producer:
                continue
            cpu = row.get('cpu_frame_id')
            if cpu is None:
                continue  # Auxiliary frames are not the changed Notes body.
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
    deliveries = {row['wayland_serial']: row
                  for row in rows(lines, 'tilo-physical-delivery ')}
    frames = []
    seen_workers = set()
    for sample in latency['samples']:
        cpu = sample['cpu_frame_id']
        if any(cpu not in group for group in (sends, workers, presents, feedback)):
            raise ValueError('Missing render stages for a verified Notes frame')
        send, worker, present, display = [group[cpu] for group in
                                           (sends, workers, presents, feedback)]
        if send.get('send_ok') is not True or worker.get('outcome') != 'Presented':
            raise ValueError('Notes frame was not successfully sent and presented')
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
        delivery = deliveries[sample['wayland_serial']]
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
        if mains:
            if cpu not in mains:
                raise ValueError('Partial Notes main-thread stage evidence')
            points = [bracket(mains[cpu], point) for point in POINTS]
            _ordered(entry, *points, before)
            pairs.update(zip(STAGES, zip(points, points[1:])))
            pairs['pre_send_gap'] = points[-1], before
        frames.append(dict(input_id=sample['input_id'], cpu_frame_id=cpu,
                           intervals_ms={key: _interval(*pair)
                                         for key, pair in pairs.items()}))
    stages = {key: {bound: distribution(
        [frame['intervals_ms'][key][index] for frame in frames], scale=1)
        for index, bound in enumerate(('lower', 'upper'))}
        for key in frames[0]['intervals_ms']}
    return dict(available=True, performance_certified=False,
                main_stages_available=bool(mains),
                clock='CLOCK_MONOTONIC', process_id=producer[0],
                observer_session=producer[1], frames=frames, stages=stages,
                latency=latency['latency'],
                limitations=['CPU stages include scheduling and logging overhead.',
                             'CPU render duration is not GPU execution duration.',
                             'KMS timestamps do not measure panel pixel response.'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--capture', type=Path, required=True,
                        help='directory produced by device_notes_latency.py')
    parser.add_argument('--out', type=Path, required=True)
    options = parser.parse_args()
    capture = json.loads((options.capture / 'capture.json').read_text())
    report = summarize_physical_notes_stages(
        (options.capture / 'timeline.log').read_text().splitlines(),
        capture['scratch_page'])
    descriptor = os.open(options.out, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'w') as output:
        json.dump(report, output, indent=2)
        output.write('\n')
    print('Verified Linux Notes frames:', len(report['frames']))


if __name__ == '__main__':
    main()
