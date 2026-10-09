#!/usr/bin/env python3
"""Score public probe logs; no microphone or private transcript input is needed.

NFKC/casefold, punctuation-to-space and whitespace collapse only. No number,
accent, Simplified/Traditional Chinese or reference-aware normalization.
Draft revisions align to the best final prefix: appended words are free.
This measures decoder revisions, not user corrections or production accuracy.
"""
import argparse
import json
import statistics
import unicodedata


def units(text, characters=False):
    text = unicodedata.normalize("NFKC", text).casefold()
    text = "".join(" " if unicodedata.category(c)[0] in "PZ" else c
                   for c in text)
    return list("".join(text.split())) if characters else text.split()


def distance(reference, hypothesis, prefix=False):
    row = list(range(len(hypothesis) + 1))
    for index, expected in enumerate(reference, 1):
        following = [index]
        for column, actual in enumerate(hypothesis, 1):
            following.append(min(following[-1] + 1, row[column] + 1,
                                 row[column - 1] + (expected != actual)))
        row = following
    return min(row) if prefix else row[-1]


def score(rows):
    result = {"cases": len(rows)}
    for key, characters in [("wer", False), ("cer", True)]:
        reference = [units(row["reference"], characters) for row in rows]
        denominator = sum(map(len, reference))
        if not denominator:
            raise ValueError("empty public reference")
        for field in ["text", "auto_text"]:
            errors = sum(distance(ref, units(row[field], characters))
                         for ref, row in zip(reference, rows))
            result[f"{field}_{key}"] = round(errors / denominator, 4)
    characters = rows[0]["language"] == "zh"
    drafts = [units(row["draft"], characters) for row in rows]
    denominator = sum(map(len, drafts))
    result["draft_prefix_revision_fraction"] = round(sum(
        distance(draft, units(row["text"], characters), prefix=True)
        for draft, row in zip(drafts, rows)) / max(1, denominator), 4)
    for field in ["draft_ms", "final_ms", "auto_ms"]:
        values = [row[field] for row in rows]
        result[field] = {"median": statistics.median(values), "max": max(values)}
    result["audio_seconds"] = round(sum(row["samples"] for row in rows) / 16000, 2)
    return result


def summarize(path):
    rows = []
    metadata = {}
    loads = []
    with open(path, encoding="utf-8") as stream:
        for line in stream:
            if line.startswith("SPEECH_BENCHMARK "):
                rows.append(json.loads(line.split(" ", 1)[1]))
            elif line.startswith("SPEECH_BENCHMARK_LOAD "):
                metadata = json.loads(line.split(" ", 1)[1])
                loads.append(metadata)
    expected = {(language, f"short-{index}") for language in
                ["en", "de", "fr", "es", "ar", "zh"] for index in range(3)}
    observed = [(row["language"], f"short-{row['id'].rsplit('-', 1)[1]}")
                for row in rows if not row["id"].startswith("long-")]
    if len(rows) != 21 or len(observed) != 18 or set(observed) != expected:
        raise ValueError("missing, duplicated or unexpected primary speech probes")
    if {row["id"] for row in rows if row["id"].startswith("long-")} != {
            "long-en", "long-fr", "long-zh"}:
        raise ValueError("missing long probes")
    if not metadata or {row["model"] for row in rows} != {metadata["model"]}:
        raise ValueError("missing or inconsistent model metadata")
    if any(load["model"] != metadata["model"] or load["threads"] != 2
           or load["memory_limit_bytes"] != 1073741824 for load in loads):
        raise ValueError("inconsistent bounded decoder qualification")
    metadata = dict(metadata, observed_primary_probes=len(rows), loads=len(loads),
                    cold_load_ms={"median": statistics.median(load["load_ms"] for load in loads),
                                  "max": max(load["load_ms"] for load in loads)})
    return {"load": metadata, "short": {
        language: score([row for row in rows if row["language"] == language
                         and not row["id"].startswith("long-")])
        for language in ["en", "de", "fr", "es", "ar", "zh"]}, "long": {
        row["language"]: score([row]) for row in rows
        if row["id"].startswith("long-")}}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logs", nargs="+")
    arguments = parser.parse_args()
    print(json.dumps([summarize(path) for path in arguments.logs], indent=2,
                     ensure_ascii=False))
