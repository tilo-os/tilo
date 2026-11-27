"""What only a daemon's connection carries besides the API: the vault's lane, which uses a
secret without handing it over, and an app's call lane."""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from typing import Any

from ._errors import Refused, refusal_of
from ._transport import CallAsk, CallFrame, Daemon, answered


def _daemon_of(workspace: Any, what: str) -> Daemon:
    """The daemon of `workspace`'s client, or the refusal of a lane only one has."""
    daemon = workspace.client.daemon
    if daemon is None:
        raise Refused(
            {
                "code": "not_permitted",
                "message": f"{what} only an app this computer's daemon started, over the "
                "connection it was handed",
            }
        )
    return daemon


async def _lane_answer(connection: Any, method: str, *args: Any) -> Any:
    """One call of a lane whose answer is JSON text: its answer, or its refusal raised."""
    text = await getattr(connection, method)(*args)
    return answered(json.loads(text))


class Vault:
    """The person's vault: a secret stays there, and a call uses it by reference.

    `web_call` has the vault add the key on the way out and answers the service's reply with the
    key struck from it; `ask` is the one way a program comes by an item it was not given. `find`
    names the item a secret an app was moved to was kept as, and `release` hands that item's
    secret to the app's own process.
    """

    def __init__(self, workspace: Any) -> None:
        self.workspace = workspace
        """The workspace whose vault it is."""

    async def web_call(
        self,
        url: str,
        key: str,
        *,
        method: str | None = None,
        headers: Mapping[str, str] | None = None,
        body: str | None = None,
        body_parts: Sequence[Mapping[str, Any]] | None = None,
        body_content: Mapping[str, Any] | None = None,
        reason: str | None = None,
    ) -> dict[str, Any]:
        """Call a service with a key the vault keeps, by its `vault.item:…` reference.

        The vault adds the key where its item says the service takes it, and only to the service
        the item belongs to. Answers `{status, headers, body}` with the key struck out.

        `body_parts` sends a multipart body: each part is `{"name", "text"}`, `{"name", "json"}`
        or `{"name", "content": {"app", "hash", "filename", "contentType"}}`. `body_content`
        sends one content reference as the whole body. Content is named by its BLAKE3 hex
        address; the vault reads the bytes from the workspace's store, so they never pass through
        the calling app.
        """
        args: dict[str, Any] = {"url": url, "key": key}
        if method is not None:
            args["method"] = method
        if headers is not None:
            args["headers"] = dict(headers)
        if body is not None:
            args["body"] = body
        if body_parts is not None:
            args["bodyParts"] = [dict(part) for part in body_parts]
        if body_content is not None:
            args["bodyContent"] = dict(body_content)
        if reason is not None:
            args["reason"] = reason
        if self.workspace.client.daemon is None:
            from ._client import run

            answer = (await run(self.workspace, {"path": "vault/vault", "action": "web-call"}, args)).get(
                "result"
            )
        else:
            daemon = _daemon_of(self.workspace, "a web call with a vault item's key is made by")
            answered_body = await _lane_answer(
                await daemon.opened(), "web_call", await self.workspace.id(), json.dumps(args)
            )
            answer = answered_body.get("result") if isinstance(answered_body, Mapping) else None
        said = answer if isinstance(answer, Mapping) else {}
        return {
            "status": int(said.get("status") or 0),
            "headers": {str(name): str(value) for name, value in (said.get("headers") or {}).items()},
            "body": str(said.get("body") or ""),
        }

    async def release(self, item: str) -> str:
        """The secret of the vault item `item` (`vault.item:…`), handed to this app's process.

        Only the process the daemon launched for an app's connection is handed one, for an item
        the workspace granted it: anything else raises `Refused`, and its person is asked once
        whether it may. The secret is answered to the caller and kept nowhere.
        """
        daemon = _daemon_of(self.workspace, "a vault item's secret is released to")
        answer = await _lane_answer(
            await daemon.opened(), "release", await self.workspace.id(), item
        )
        secret = answer.get("secret") if isinstance(answer, Mapping) else None
        if not isinstance(secret, str):
            raise Refused({"code": "failed", "message": "the vault answered a release with no secret"})
        return secret

    async def find(self, app: str, purpose: str) -> str | None:
        """The reference of the vault item kept for the app and purpose named, or `None` when
        the vault keeps none. It releases nothing, so the person is not asked."""
        daemon = _daemon_of(self.workspace, "a vault item is found by")
        answer = await _lane_answer(
            await daemon.opened(), "find", await self.workspace.id(), app, purpose
        )
        reference = answer.get("reference") if isinstance(answer, Mapping) else None
        return reference if isinstance(reference, str) else None

    async def ask(
        self,
        kind: str,
        belongs_to: Sequence[str],
        *,
        username: str | None = None,
        sent_in: str | None = None,
    ) -> str | None:
        """Ask the person for a secret of `kind` that belongs to `belongs_to`.

        `belongs_to` names each site or server (`https://example.com`,
        `imap://mail.example.com:993`) the secret is ever handed to, and `sent_in` says where a
        key rides in a request (`basic`, `header:<name>`, `query:<name>`) when not as a bearer
        token. Answers the `vault.item:…` reference it is kept under, or `None` when they
        declined.
        """
        from ._client import run

        args: dict[str, Any] = {"kind": kind, "belongsTo": list(belongs_to)}
        if username is not None:
            args["username"] = username
        if sent_in is not None:
            args["sentIn"] = sent_in
        answer = await run(self.workspace, {"path": "vault/vault", "action": "ask"}, args)
        result = answer.get("result") if isinstance(answer, Mapping) else None
        reference = result.get("reference") if isinstance(result, Mapping) else None
        return reference if isinstance(reference, str) else None


class CallLane:
    """The call lane of an app's own connection: how a bridge app carries one outside service's
    call into one of the workspace's calls.

    The app says which outside call it carries into which workspace call (`carry`), who is in it
    (`report`), and hands over each Opus frame its people say (`heard`); the device gives back
    what the workspace's people say (`said`) and where it wants the app in the outside call
    (`asked`). Any other connection's calls raise `Refused`.
    """

    def __init__(self, workspace: Any) -> None:
        self.workspace = workspace
        """The workspace whose call it carries."""
        self._frames: list[CallFrame] = []
        self._asked: CallAsk | None = None

    async def _connection(self) -> Any:
        daemon = _daemon_of(self.workspace, "a call is carried by")
        connection = await daemon.opened()
        if getattr(connection, "carry_call", None) is None:
            raise Refused(
                {
                    "code": "not_permitted",
                    "message": "a call is carried only by an app this computer's daemon started, "
                    "over the connection it was handed",
                }
            )
        return connection

    async def carry(self, pair: Mapping[str, str] | None) -> None:
        """Carry the outside call `pair["outside"]` into the workspace call `pair["tilo"]`, or let
        go of the one carried (`None`), which leaves the workspace's call."""
        connection = await self._connection()
        await _lane_answer(connection, "carry_call", json.dumps(None if pair is None else dict(pair)))

    async def report(self, report: Mapping[str, Any]) -> None:
        """Say who is in the outside calls and where the app stands in them: `{"calls":
        [{"call", "people"}], "link": {...}, "status"}`."""
        connection = await self._connection()
        await _lane_answer(connection, "report_call", json.dumps(dict(report)))

    async def heard(self, speaker: str, opus: bytes) -> None:
        """Hand the workspace's call one 20 ms Opus frame the outside account `speaker` said."""
        daemon = _daemon_of(self.workspace, "a call is carried by")
        connection = await daemon.opened()
        heard = getattr(connection, "hear_call", None)
        if heard is None:
            raise Refused(
                {
                    "code": "not_permitted",
                    "message": "a call is carried only by an app this computer's daemon started, "
                    "over the connection it was handed",
                }
            )
        refusal = heard(speaker, bytes(opus))
        if refusal is not None:
            raise refusal_of(json.loads(refusal))

    async def said(self) -> list[CallFrame]:
        """The frames the workspace's people said since the last time, oldest first."""
        await self._take()
        frames, self._frames = self._frames, []
        return frames

    async def asked(self) -> CallAsk | None:
        """Where the device last asked the app to be in the outside call, once: `None` when it
        asked nothing new since."""
        await self._take()
        asked, self._asked = self._asked, None
        return asked

    async def _take(self) -> None:
        daemon = _daemon_of(self.workspace, "a call is carried by")
        connection = await daemon.opened()
        said = getattr(connection, "said_call", None)
        if said is None:
            raise Refused(
                {
                    "code": "not_permitted",
                    "message": "a call is carried only by an app this computer's daemon started, "
                    "over the connection it was handed",
                }
            )
        answer = answered(json.loads(said())) or {}
        for frame in answer.get("frames") or []:
            self._frames.append(
                CallFrame(
                    speaker=str(frame["speaker"]),
                    opus=bytes(frame["opus"]),
                    skipped=int(frame.get("skipped", 0)),
                )
            )
        asked = answer.get("asked")
        if asked is not None:
            self._asked = CallAsk(
                call=asked.get("call"),
                status=asked.get("status"),
            )


__all__ = ["CallAsk", "CallFrame", "CallLane", "Vault"]
