"""A recorded model for Tilo's Tasks agent: the fake Anthropic Messages endpoint of
scripts/perf/agent_task.py, answering each turn from a recorded script instead of a policy.

A script is JSON: `{"request": ..., "turns": [{"expect": [...], "blocks": [...]}]}`. Turn n
answers the n-th model request with its blocks (text and `tool_use`, as the Messages API
streams them). `expect` lists text the results of the previous turn's tool calls must hold, so
a replay that drifts from what Tilo answers fails at the turn it drifts, with what Tilo said,
instead of finishing on a script that no longer fits.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "scripts" / "perf"))
from agent_task import FakeModel, PolicyError, tool  # noqa: E402


def tool_results(message: dict[str, Any]) -> str:
    """Every tool result a user message carries, as one text."""
    content = message.get("content")
    if isinstance(content, str):
        return content
    out = []
    for block in content or []:
        if block.get("type") != "tool_result":
            continue
        inner = block.get("content")
        if isinstance(inner, str):
            out.append(inner)
        else:
            out.extend(part.get("text", "") for part in inner or [] if isinstance(part, dict))
    return "\n".join(out)


# What the first message of a task's own loop holds, and no transform's request does.
TASK_MARK = "Operations published right now"


def text_of(message: dict[str, Any]) -> str:
    content = message.get("content")
    if isinstance(content, str):
        return content
    return "\n".join(block.get("text", "") for block in content or [] if isinstance(block, dict))


class Replay:
    """Answers model requests from a recorded script, checking each turn's premise."""

    def __init__(self, script: dict[str, Any]):
        self.turns = script["turns"]
        self.title = script.get("title", script["request"])
        self.responds = 0
        self.requests: list[list[dict[str, Any]]] = []
        self.results: list[str] = []

    def respond(self, messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
        # The composer first asks whether the request is answered at once or is work for a
        # task (the agent's `respond` transform); this script's request is work.
        if not any(TASK_MARK in tool_results(message) or TASK_MARK in text_of(message)
                   for message in messages):
            self.responds += 1
            return [{"type": "text", "text": json.dumps({"result": {"work": self.title}})}]
        self.requests.append(messages)
        turn = sum(1 for message in messages if message.get("role") == "assistant")
        if turn >= len(self.turns):
            raise PolicyError(f"the script has {len(self.turns)} turns; the agent asked for turn {turn + 1}")
        said = tool_results(messages[-1]) if turn else ""
        self.results.append(said)
        for expected in self.turns[turn].get("expect", []):
            if expected not in said:
                raise PolicyError(f"turn {turn + 1} expects {expected!r} in what Tilo answered: {said[:1500]!r}")
        blocks = []
        for block in self.turns[turn]["blocks"]:
            if block["type"] == "tool_use":
                blocks.append(tool(block["name"], block["input"]))
            else:
                blocks.append(dict(block))
        return blocks


def serve(script_path: Path) -> tuple[FakeModel, Replay]:
    replay = Replay(json.loads(script_path.read_text()))
    return FakeModel(replay.respond), replay
