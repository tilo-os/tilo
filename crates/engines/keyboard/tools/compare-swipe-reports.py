#!/usr/bin/env python3
"""Paired donor-cluster bootstrap of public aggregate swipe reports, remotely."""
import argparse
import json
import random
from pathlib import Path


def compare(baseline, candidate, repetitions=10000):
    for key in ["revision", "split", "fixture_sha256"]:
        if baseline["manifest"][key] != candidate["manifest"][key]:
            raise ValueError(f"reports describe different fixtures: {key}")
    before = baseline["statistics"]["sessions"]
    after = candidate["statistics"]["sessions"]
    if not after or not set(after).issubset(before):
        raise ValueError("no matching donor sessions")
    result = {"method": "paired donor-cluster percentile bootstrap, gesture-weighted",
              "seed": 483217, "repetitions": repetitions, "sessions": len(after),
              "baseline_alignment": baseline["alignment"],
              "candidate_alignment": candidate["alignment"],
              "candidate_prior_hundredths": candidate["elastic_prior_hundredths"],
              "fixture_sha256": candidate["manifest"]["fixture_sha256"],
              "metrics": {}}
    for mode in ["literal", "lexical"]:
        for rank in ["top1_correct", "top3_correct", "top8_correct"]:
            pairs = []
            for session, stats in after.items():
                old, new = before[session][mode], stats[mode]
                if old["eligible"] != new["eligible"] or old["in_vocabulary"] != new["in_vocabulary"]:
                    raise ValueError("eligibility changed within a paired session")
                pairs.append((new["eligible"], old[rank], new[rank]))
            denominator = sum(p[0] for p in pairs)
            if not denominator:
                raise ValueError("no eligible gestures")
            rng = random.Random(result["seed"])
            draws = []
            for _ in range(repetitions):
                count = delta = 0
                for _ in pairs:
                    eligible, old, new = pairs[rng.randrange(len(pairs))]
                    count += eligible
                    delta += new - old
                if not count:
                    raise ValueError("bootstrap replicate contains no eligible gestures")
                draws.append(delta / count)
            draws.sort()
            result["metrics"][f"{mode}_{rank.removesuffix('_correct')}"] = {
                "eligible": denominator,
                "baseline_accuracy": sum(p[1] for p in pairs) / denominator,
                "candidate_accuracy": sum(p[2] for p in pairs) / denominator,
                "paired_gain": sum(p[2] - p[1] for p in pairs) / denominator,
                "gain_95_percent_interval": [draws[repetitions * 25 // 1000],
                                              draws[repetitions * 975 // 1000]],
            }
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("out", type=Path)
    args = parser.parse_args()
    result = compare(json.loads(args.baseline.read_text()), json.loads(args.candidate.read_text()))
    args.out.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
