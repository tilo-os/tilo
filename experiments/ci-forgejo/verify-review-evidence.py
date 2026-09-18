#!/usr/bin/env python3
"""Check the recorded live review-invalidation evidence, without remote writes."""
import json
from datetime import datetime
from email.utils import parsedate_to_datetime
from pathlib import Path


ROOT = Path(__file__).parent / "results/review-head-20260918"


def read(name):
    return json.loads((ROOT / name).read_text(encoding="utf-8-sig"))


def rows(value):
    return value if isinstance(value, list) else [value]


def main():
    setup = read("setup.json")
    update = read("head-update.json")
    policy = read("protection.json")
    assert policy["required_approvals"] == 1
    assert policy["apply_to_admins"] and policy["dismiss_stale_approvals"]
    assert policy["enable_approvals_whitelist"]
    assert policy["approvals_whitelist_username"] == ["claude"]
    assert policy["enable_status_check"]
    assert policy["status_check_contexts"] == ["pilot/ready"]
    first = rows(read("first-review.json"))[0]
    assert first["user"] != setup["author"]
    assert first["user"] == "claude" and first["state"] == "APPROVED"
    assert first["commit_id"] == setup["head"] == update["previous_head"]
    assert first["official"] and not first["dismissed"]
    stale = rows(read("after-update-reviews.json"))[0]
    assert stale["id"] == first["id"] and stale["dismissed"]
    assert stale["user"] == first["user"]
    assert stale["commit_id"] == first["commit_id"]
    assert update["new_head"] != stale["commit_id"]
    for name, status_name, sha in (
        ("before-review.http", "original-status.json", setup["head"]),
        ("stale-review-merge.http", "updated-status.json", update["new_head"]),
    ):
        response = (ROOT / name).read_text(encoding="utf-8-sig")
        assert response.startswith("HTTP/1.1 405\n"), name
        assert "Does not have enough approvals" in response, name
        date = next(line.split(":", 1)[1].strip() for line in response.splitlines()
                    if line.lower().startswith("date:"))
        attempted_at = parsedate_to_datetime(date)
        status = read(status_name)
        assert status["sha"] == sha and status["state"] == "success", status_name
        checks = [item for item in status["statuses"] if item["context"] == "pilot/ready"]
        assert len(checks) == 1 and checks[0]["status"] == "success", status_name
        created_at = datetime.fromisoformat(checks[0]["created_at"].replace("Z", "+00:00"))
        updated_at = datetime.fromisoformat(checks[0]["updated_at"].replace("Z", "+00:00"))
        collected_at = datetime.fromisoformat(status["collected_at"])
        assert created_at <= updated_at <= attempted_at <= collected_at, status_name
    fresh = [review for review in rows(read("fresh-reviews.json"))
             if review["state"] == "APPROVED" and not review["dismissed"]
             and review["official"] and review["user"] == "claude"
             and review["commit_id"] == update["new_head"]]
    assert len(fresh) == 1 and fresh[0]["id"] != first["id"]
    assert (ROOT / "fresh-review-merge.http").read_text(encoding="utf-8-sig").startswith("HTTP/1.1 200\n")
    result = read("result.json")
    assert result["merged"]
    assert result["merge_commit_sha"] == result["final_main"] == result["expected_head"] == update["new_head"]
    assert result["reviewer_repository_permission"] == "owner"
    permission = read("reviewer-permission.json")
    assert permission["login"] == first["user"] and permission["id"] == 5
    assert permission["permission"] == permission["role_name"] == "owner"
    print("Live review evidence consistent: stale approval dismissed, stale-reviewed head refused, fresh-reviewed exact head merged.")
    print("Scope: distinct owner identities and synthetic checks; not ordinary-user isolation or independent code-review quality.")


if __name__ == "__main__":
    main()
