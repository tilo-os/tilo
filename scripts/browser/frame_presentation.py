#!/usr/bin/env python3
"""Join a single Rust process's browser receipts/scenes to Metal presentation.

Measures receipt-to-display only. It does not infer input causality or replace
raw logs. Unpresented receipts and rejected evidence remain explicit.
"""
import argparse
from collections import Counter, defaultdict
import json
import math
from pathlib import Path

PREFIXES = {'qos-webview-received', 'qos-webview-scene', 'qos-display-frame'}


def number(value):
    return type(value) in (int, float) and math.isfinite(value)


def identity(value):
    return type(value) is int and value > 0


def frame_index(value):
    return type(value) is int and value >= 0


def analyze(rows):
    issues = Counter()
    receipts = {}
    scenes = {}
    presentations = defaultdict(list)
    duplicate_receipts = set()
    duplicate_scenes = set()
    processes = set()
    for prefix, row in rows:
        if not isinstance(row, dict):
            issues['invalid_record'] += 1
            continue
        if prefix in {'qos-webview-received', 'qos-webview-scene'}:
            pid = row.get('process_id')
            if not identity(pid):
                issues['invalid_process'] += 1
                continue
            processes.add(pid)
        if prefix == 'qos-webview-received':
            fid = row.get('frame_id')
            lo, hi = row.get('received_host_lo_s'), row.get('received_host_hi_s')
            if not identity(fid):
                issues['invalid_frame_id'] += 1
                continue
            if fid in receipts:
                duplicate_receipts.add(fid)
                issues['duplicate_receipt'] += 1
            # Keep invalid receipts represented so coverage cannot silently improve.
            receipts[fid] = row
            if (row.get('host_clock') != 'mach_absolute_seconds' or
                    not number(lo) or not number(hi) or lo <= 0 or hi < lo):
                duplicate_receipts.add(fid)
                issues['invalid_receipt_clock'] += 1
        elif prefix == 'qos-webview-scene':
            cpu, ids = row.get('cpu_frame_id'), row.get('webview_frame_ids')
            if not frame_index(cpu):
                issues['missing_cpu_frame_id'] += 1
                continue
            if cpu in scenes:
                duplicate_scenes.add(cpu)
                issues['duplicate_scene'] += 1
            scenes[cpu] = row
            if (row.get('overflow') is not False or not isinstance(ids, list) or
                    len(ids) > 64 or any(not identity(fid) for fid in ids) or
                    len(ids) != len(set(ids))):
                duplicate_scenes.add(cpu)
                issues['invalid_or_overflowed_scene'] += 1
        elif prefix == 'qos-display-frame':
            cpu = row.get('cpu_frame_id')
            if frame_index(cpu):
                presentations[cpu].append(row)
    mixed = len(processes) > 1
    if mixed:
        issues['mixed_processes'] += 1
    samples = {}
    for cpu, scene in scenes.items():
        if mixed or cpu in duplicate_scenes:
            continue
        displays = presentations.get(cpu, [])
        if not displays:
            continue  # Recorded but dropped/unobserved scene: not zero latency.
        if len(displays) != 1:
            issues['ambiguous_presentation'] += 1
            continue
        display = displays[0]
        stamp = display.get('presented_time_s')
        if (display.get('backend') != 'metal' or not number(stamp) or stamp <= 0 or
                not identity(display.get('worker_id')) or
                not frame_index(display.get('worker_frame_ix')) or
                type(display.get('drawable_id')) is not int or display['drawable_id'] < 0):
            issues['invalid_presentation'] += 1
            continue
        for fid in scene['webview_frame_ids']:
            if fid not in receipts:
                issues['missing_receipt'] += 1
                continue
            if fid in duplicate_receipts:
                continue
            receipt = receipts[fid]
            lo, hi = receipt['received_host_lo_s'], receipt['received_host_hi_s']
            if stamp < hi:
                issues['presentation_before_receipt'] += 1
                continue
            if fid not in samples or stamp < samples[fid]['presented_time_s']:
                samples[fid] = dict(frame_id=fid, cpu_frame_id=cpu,
                    tab_id=receipt.get('tab_id'), presented_time_s=stamp,
                    latency_low_ms=(stamp-hi)*1000, latency_high_ms=(stamp-lo)*1000)
    ordered = [samples[fid] for fid in sorted(samples)]
    def percentile(key, quantile):
        values = sorted(row[key] for row in ordered)
        if not values:
            return None
        position = (len(values)-1)*quantile
        low = math.floor(position)
        high = math.ceil(position)
        return values[low] + (values[high]-values[low])*(position-low)
    return dict(metric='browser_frame_receipt_to_display', input_causality_verified=False,
        process_id=next(iter(processes)) if len(processes) == 1 else None,
        receipt_count=len(receipts), scene_count=len(scenes), sample_count=len(ordered),
        unpresented_frame_ids=sorted(set(receipts)-set(samples)), issues=dict(issues),
        samples=ordered, latency_ms={name: dict(low=percentile('latency_low_ms', q),
            high=percentile('latency_high_ms', q)) for name, q in [('p50', .5), ('p95', .95), ('p99', .99)]})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('log', type=Path, help='stderr from exactly one Rust browser process')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    rows = []
    with args.log.open() as source:
        for line in source:
            prefix, separator, payload = line.partition(' ')
            if separator and prefix in PREFIXES:
                try:
                    row = json.loads(payload)
                except (ValueError, RecursionError):
                    row = None
                rows.append((prefix, row))
    result = analyze(rows)
    output = json.dumps(result, indent=2, allow_nan=False)+'\n'
    if args.output:
        args.output.write_text(output)
    else:
        print(output, end='')
    return 0 if result['samples'] and not result['issues'] else 1

if __name__ == '__main__':
    raise SystemExit(main())
