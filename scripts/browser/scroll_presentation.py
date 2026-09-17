#!/usr/bin/env python3
"""Controlled fixture only: join uniquely sized wheel inputs to painted markers.

This measures app-dispatch to scroll-marker presentation, not hardware input
latency. Use fixtures/scroll-response.html, one tab, unique vertical line wheel
deltas, zoom 100%, and QOS_WEBVIEW_SCROLL_MARKER=1. Preserve raw logs.
"""
import argparse
from collections import Counter
import json
import math
import struct
from pathlib import Path
from frame_presentation import analyze as analyze_frames, number, PREFIXES


def analyze(rows):
    frames = analyze_frames(rows)
    issues = Counter(frames['issues'])
    windows = [row for prefix, row in rows if prefix == 'qos-native-window']
    scale = windows[0].get('scale_factor') if len(windows) == 1 and isinstance(windows[0], dict) else None
    if not number(scale) or scale <= 0:
        issues['invalid_window_scale'] += 1
    inputs, deltas = {}, {}
    invalid = set()
    for prefix, row in rows:
        if prefix != 'qos-native-input': continue
        if not isinstance(row, dict) or not isinstance(row.get('input'), dict):
            issues['invalid_input_record'] += 1
            continue
        if row['input'].get('kind') != 'wheel': continue
        key = row.get('id')
        if type(key) is not int or key < 0:
            issues['invalid_input_id'] += 1
            continue
        if key in inputs:
            issues['duplicate_input_id'] += 1
            invalid.add(key)
        inputs[key] = row
        lo, hi = row.get('dispatch_host_lo_s'), row.get('dispatch_host_hi_s')
        dy, dx = row['input'].get('dy'), row['input'].get('dx')
        if (row.get('host_clock') != 'mach_absolute_seconds' or not number(lo) or
                not number(hi) or lo <= 0 or hi < lo or not number(dy) or dx != 0 or
                not number(scale) or scale <= 0):
            issues['invalid_input_clock_or_delta'] += 1
            invalid.add(key)
            continue
        # NativeScenario Wheel is LineDelta. WebView multiplies by 40 CSS px
        # and device scale, narrows to f32, then CEF divides by scale/truncates.
        # DOM deltaY has the opposite sign from SendMouseWheelEvent.
        try:
            physical = struct.unpack('f', struct.pack('f', dy * 40 * scale))[0]
            delta = -math.trunc(physical / scale)
        except (OverflowError, ValueError):
            issues['unrepresentable_wheel_delta'] += 1
            invalid.add(key)
            continue
        if not delta or abs(delta) > 32767:
            issues['unrepresentable_wheel_delta'] += 1
            invalid.add(key)
        elif delta in deltas:
            issues['ambiguous_wheel_delta'] += 1
            invalid.update([key, deltas[delta]])
        else:
            deltas[delta] = key
    receipts = {row['frame_id']: row for prefix, row in rows
                if prefix == 'qos-webview-received' and isinstance(row, dict) and
                type(row.get('frame_id')) is int}
    tabs = {row.get('tab_id') for row in receipts.values()}
    if len(tabs) != 1:
        issues['requires_single_tab'] += 1
        invalid.update(inputs)
    # A marker seen before dispatch invalidates the causal claim even if that
    # frame was dropped. A later animation can repaint the same old marker.
    for prefix, receipt in rows:
        if prefix != 'qos-webview-received' or not isinstance(receipt, dict): continue
        marker = receipt.get('scroll_marker_delta')
        if type(marker) is not int or marker not in deltas: continue
        key = deltas[marker]
        if key in invalid: continue
        lo, hi = receipt.get('received_host_lo_s'), receipt.get('received_host_hi_s')
        if (receipt.get('host_clock') != 'mach_absolute_seconds' or
                not number(lo) or not number(hi) or lo <= 0 or hi < lo):
            issues['invalid_marker_clock'] += 1
            invalid.add(key)
        elif lo < inputs[key]['dispatch_host_hi_s']:
            issues['marker_before_input'] += 1
            invalid.add(key)
    samples = {}
    for frame in frames['samples']:
        receipt = receipts[frame['frame_id']]
        marker = receipt.get('scroll_marker_delta')
        if type(marker) is not int or marker not in deltas: continue
        key = deltas[marker]
        if key in invalid: continue
        row = inputs[key]
        stamp = frame['presented_time_s']
        sample = dict(input_id=key, frame_id=frame['frame_id'], cpu_frame_id=frame['cpu_frame_id'],
                      marker_delta=marker, presented_time_s=stamp, warmup=row.get('warmup', False),
                      latency_low_ms=(stamp-row['dispatch_host_hi_s'])*1000,
                      latency_high_ms=(stamp-row['dispatch_host_lo_s'])*1000)
        if key not in samples or stamp < samples[key]['presented_time_s']:
            samples[key] = sample
    return dict(metric='controlled_wheel_dispatch_to_scroll_marker_display',
                scope='controlled fixture; synthetic app input; not hardware latency',
                input_count=len(inputs), samples=[samples[k] for k in sorted(samples)],
                unanswered_input_ids=sorted(set(inputs)-set(samples)), issues=dict(issues),
                frame_evidence=frames)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('log', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    rows = []
    prefixes = PREFIXES | {'qos-native-input', 'qos-native-window'}
    for line in args.log.read_text().splitlines():
        prefix, _, payload = line.partition(' ')
        if prefix in prefixes:
            try: row = json.loads(payload)
            except (ValueError, RecursionError): row = None
            rows.append((prefix, row))
    result = analyze(rows)
    output = json.dumps(result, indent=2, allow_nan=False)+'\n'
    if args.output: args.output.write_text(output)
    else: print(output, end='')
    return 0 if result['samples'] and not result['issues'] and not result['unanswered_input_ids'] else 1

if __name__ == '__main__': raise SystemExit(main())
