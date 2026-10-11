#!/usr/bin/env python3
"""Check campaign smoke runs against the committed baseline.

Each run directory is one `campaign_bench` run (TILO_BENCH_OUT): its
`timing.json` and the per-call `usage.jsonl` (TILO_AGENT_USAGE_LOG). Time is
first task to last, setup excluded; cost is priced from the logged usage at
the list prices below, the service tier's factor applied.

    tools/agent-bench/check.py --model gpt-6-luna run-1 run-2 run-3

Exits non-zero when the median time or cost exceeds the baseline by more
than its allowance (tools/agent-bench/baseline.json).
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import statistics
import sys

# USD per million tokens at the standard tier.
PRICES = {
    'gpt-6-luna': {'input': 0.10, 'cached': 0.01, 'write': 0.125, 'output': 0.50},
    'gpt-6-sol': {'input': 2.0, 'cached': 0.20, 'write': 2.5, 'output': 10.0},
    'gpt-6-astra': {'input': 10.0, 'cached': 1.0, 'write': 12.5, 'output': 50.0},
    'claude-haiku-5-5': {'input': 0.10, 'cached': 0.01, 'write': 0.125, 'output': 0.50},
    'claude-sonnet-5-5': {'input': 2.0, 'cached': 0.10, 'write': 2.5, 'output': 10.0},
}
TIER_FACTOR = {'flex': 0.5, 'batch': 0.5, 'fast': 2.0, 'priority': 2.0}


def call_cost(row: dict) -> float:
    price = PRICES.get(row.get('model', ''))
    usage = row.get('usage') or {}
    if price is None or not usage:
        return 0.0
    if 'cache_read_input_tokens' in usage:  # Anthropic: input excludes cache traffic
        cached = usage.get('cache_read_input_tokens') or 0
        writes = usage.get('cache_creation_input_tokens') or 0
        plain = usage.get('input_tokens') or 0
    else:
        details = usage.get('input_tokens_details') or {}
        cached = details.get('cached_tokens') or 0
        writes = details.get('cache_write_tokens') or 0
        plain = max(0, (usage.get('input_tokens') or 0) - cached - writes)
    output = usage.get('output_tokens') or 0
    cost = (plain * price['input'] + cached * price['cached'] + writes * price['write']
            + output * price['output']) / 1e6
    return cost * TIER_FACTOR.get(row.get('service_tier') or '', 1.0)


def run_numbers(run: Path) -> tuple[float, float]:
    timing = json.loads((run / 'timing.json').read_text())
    seconds = timing['finished_at'] - timing['started_at']
    usage = run / 'usage.jsonl'
    rows = [json.loads(line) for line in usage.read_text().splitlines() if line.strip()] if usage.is_file() else []
    return seconds, sum(call_cost(row) for row in rows)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--model', required=True)
    parser.add_argument('--baseline', type=Path, default=Path(__file__).with_name('baseline.json'))
    parser.add_argument('runs', nargs='+', type=Path)
    args = parser.parse_args()
    numbers = [run_numbers(run) for run in args.runs]
    seconds = statistics.median(n[0] for n in numbers)
    cost = statistics.median(n[1] for n in numbers)
    print(f'{args.model}: median {seconds:.1f} s, ${cost:.4f} over {len(numbers)} runs')
    baseline = json.loads(args.baseline.read_text()).get(args.model)
    if baseline is None:
        print(f'no baseline for {args.model}; nothing to compare')
        return 0
    failures = []
    if seconds > baseline['seconds'] * baseline['time_allowance']:
        failures.append(f"time {seconds:.1f} s exceeds {baseline['seconds']} s x {baseline['time_allowance']}")
    if cost > baseline['cost_usd'] * baseline['cost_allowance']:
        failures.append(f"cost ${cost:.4f} exceeds ${baseline['cost_usd']} x {baseline['cost_allowance']}")
    for failure in failures:
        print('regression:', failure)
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
