"""Verify retained live Forgejo empty-matrix and failed-selection records."""
import json
from pathlib import Path

ROOT = Path(__file__).parent / "results/empty-matrix-20260918"


def main():
    setup = json.loads((ROOT / "sentinel-setup.json").read_text())
    for fixture in setup["fixtures"]:
        mode = fixture["mode"]
        run = json.loads((ROOT / (mode + "-sentinel-run.json")).read_text())
        jobs = json.loads((ROOT / (mode + "-sentinel-jobs.json")).read_text())
        assert run["repository"]["full_name"] == setup["repo"]
        assert run["commit_sha"] == fixture["sha"]
        assert run["workflow_id"] == "empty-matrix.yml"
        assert len(jobs) == 3
        by_name = {job["name"].split(" (")[0]: job for job in jobs}
        assert set(by_name) == {"select", "affected", "checks"}
        for job in jobs:
            assert job["run_id"] == run["id"]
            assert job["repo_id"] == run["repository"]["id"]
        expected = "success" if mode == "design" else "failure"
        assert run["status"] == expected, run
        assert by_name["select"]["status"] == expected
        assert by_name["checks"]["status"] == expected
        if mode == "design":
            assert by_name["affected"]["status"] == "skipped"
            assert by_name["affected"]["task_id"] == 0
            assert by_name["checks"]["task_id"] > 0
        else:
            # This Forgejo version propagates a failed selector through the
            # incomplete matrix without scheduling the aggregate's Bash.
            assert by_name["affected"]["status"] == "failure"
            assert by_name["checks"]["task_id"] == 0
    print("PASS: sentinel skip aggregates successfully; failed selection fails closed before aggregate execution")


if __name__ == "__main__":
    main()
