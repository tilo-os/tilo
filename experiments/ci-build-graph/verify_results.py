#!/usr/bin/env python3
"""Fail if the recorded execution evidence contradicts the reported conclusions."""
import json
from pathlib import Path


def read(name):
    return json.loads((Path(__file__).parent / "results" / name).read_text(encoding="utf-8-sig"))


def main():
    actions = read("bazel-actions.json")
    actual = lambda name: {a["targetLabel"] for a in actions[name + "-actions.json"] if not a["cacheHit"]}
    assert actual("warm") == set()
    assert actual("docs_change") == set()
    assert actual("app_change") == {"//:calc", "//:app_a"}
    assert actual("shared_change") == {"//:atomic_file", "//:app_a", "//:app_b"}
    assert actual("package_change") == {"//:app_a_package", "//:app_b_package"}
    fresh = actions["fresh_output_base-actions.json"]
    assert len(fresh) == 9 and all(a["cacheHit"] and a["exitCode"] == 0 for a in fresh)
    assert {"//:calc", "//:order", "//:atomic_file", "//:app_a", "//:app_b"} <= actual("compiler_flag_change")
    probe = read("build-probe.json")
    assert all(r["exit"] == 0 for case in probe["builds"].values() for r in case.values())
    assert probe["calculator_tests_bazel"]["exit"] == probe["calculator_tests_cargo"]["exit"] == 0
    assert probe["planner"]["calculator"]["app_count"] == 1
    assert probe["planner"]["toolkit"]["app_count"] == 53
    cache = read("sccache-probe.json")
    for worker, hits, misses in [("worker-a", 0, 3), ("worker-b", 0, 3), ("worker-a-repeat", 3, 0)]:
        stats = cache[worker]["stats"]["stats"]
        assert stats["cache_hits"]["counts"].get("Rust", 0) == hits
        assert stats["cache_misses"]["counts"].get("Rust", 0) == misses
        assert stats["not_cached"]["crate-type"] == 2
    model = read("queue-model.json")["policies"]
    assert model["latest-pending"]["peak_pending"] == 1
    assert model["latest-pending"]["builds_completed"] == 8
    assert model["latest-pending"]["latest_revision_ready_at"] == 96
    assembly = read("assembly-inputs.json")["results"]
    for arch, cases in assembly.items():
        assert cases["baseline"]["roots_sha256"] == cases["config_copy_only"]["roots_sha256"]
        assert cases["baseline"]["recipe_text_sha256"] != cases["config_copy_only"]["recipe_text_sha256"]
        assert (cases["baseline"]["roots_sha256"] == cases["arm64_package_input"]["roots_sha256"]) == (arch == "x86_64")
    print("Recorded planner, cache, invalidation, RPM-input and queue-model assertions passed.")


if __name__ == "__main__":
    main()
