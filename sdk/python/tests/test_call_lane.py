"""An app's call lane through the daemon's transport, as a bridge program in Python uses it.

The native part stands in for the daemon here: it answers the JSON text the extension
answers, so the lane is driven through the SDK's public surface down to that boundary.
"""

from __future__ import annotations

import json
from typing import Any

import pytest

from tilo import AsyncTilo, CallAsk, CallFrame, DaemonTransport, Refused, Tilo


class Daemon:
    """The extension's daemon, as an app's held connection answers it."""

    workspace = "workspace-1"

    def __init__(self, refuse: str | None = None) -> None:
        self.refuse = refuse
        self.carried: list[Any] = []
        self.reports: list[Any] = []
        self.heard: list[tuple[str, bytes]] = []
        self.frames: list[dict[str, Any]] = []
        self.ask: dict[str, Any] | None = None

    def _refusal(self) -> dict[str, Any]:
        return {"error": {"code": "not_permitted", "message": self.refuse}}

    async def carry_call(self, pair: str) -> str:
        if self.refuse:
            return json.dumps({"refused": self._refusal()})
        self.carried.append(json.loads(pair))
        return json.dumps({"answer": {}})

    async def report_call(self, report: str) -> str:
        self.reports.append(json.loads(report))
        return json.dumps({"answer": {}})

    def hear_call(self, speaker: str, opus: bytes) -> str | None:
        if self.refuse:
            return json.dumps(self._refusal())
        self.heard.append((speaker, opus))
        return None

    def said_call(self) -> str:
        frames, self.frames = self.frames, []
        asked, self.ask = self.ask, None
        return json.dumps({"answer": {"frames": frames, "asked": asked}})

    async def call(self, *args: Any) -> str:
        return json.dumps({"answer": {}})


async def test_a_carried_call_goes_both_ways_and_says_where_the_app_is_wanted() -> None:
    daemon = Daemon()
    async with AsyncTilo(transport=DaemonTransport(daemon)) as tilo:
        lane = tilo.workspace().call_lane
        await lane.carry({"outside": "458", "tilo": "voice-tl"})
        assert daemon.carried == [{"outside": "458", "tilo": "voice-tl"}]

        report = {
            "calls": [{"call": "458", "people": [{"account": "7001", "name": "Dana"}]}],
            "link": {"link": "joined", "call": "458"},
        }
        await lane.report(report)
        assert daemon.reports == [report]

        await lane.heard("7001", b"\x01\x02\x03")
        assert daemon.heard == [("7001", b"\x01\x02\x03")]

        # One take hands over the frames and the ask: the frames are said, the ask is kept.
        daemon.frames = [
            {"speaker": "aa", "opus": [7, 8, 9], "skipped": 0},
            {"speaker": "mixed", "opus": [4], "skipped": 2},
        ]
        daemon.ask = {"call": "458", "status": "On Tilo: Grace"}
        assert await lane.said() == [
            CallFrame(speaker="aa", opus=b"\x07\x08\x09"),
            CallFrame(speaker="mixed", opus=b"\x04", skipped=2),
        ]
        assert await lane.asked() == CallAsk(call="458", status="On Tilo: Grace")
        assert await lane.asked() is None

        await lane.carry(None)
        assert daemon.carried[-1] is None


async def test_a_workspace_that_grants_no_call_refuses_the_lane_with_why() -> None:
    daemon = Daemon(refuse="this workspace grants the bridge no call")
    async with AsyncTilo(transport=DaemonTransport(daemon)) as tilo:
        lane = tilo.workspace().call_lane
        with pytest.raises(Refused) as refused:
            await lane.carry({"outside": "458", "tilo": "voice-tl"})
        assert "grants the bridge no call" in str(refused.value)
        with pytest.raises(Refused):
            await lane.heard("7001", b"\x01")


def test_a_program_that_is_no_app_has_no_lane() -> None:
    class Plain:
        workspace = "workspace-1"

        async def call(self, *args: Any) -> Any:
            return {}

        async def aclose(self) -> None:
            return None

    with Tilo(transport=Plain()) as tilo:
        with pytest.raises(Refused) as refused:
            tilo.workspace().call_lane.carry({"outside": "458", "tilo": "voice-tl"})
        assert refused.value.code == "not_permitted"
