"""Bounded metadata discovery over the existing semantic action door."""
import json
import re
import time
import unicodedata
import uuid
from .vm import VmError


def semantic_identity(row):
    identity = row.get("semantic_identity")
    if not isinstance(identity, dict):
        return None
    kind, key = identity.get("kind"), identity.get("key")
    if (not isinstance(kind, str) or not re.fullmatch(r"[A-Za-z0-9._-]{1,128}", kind)
            or not isinstance(key, str) or not key or len(key.encode()) > 4096
            or any(unicodedata.category(c) == "Cc" for c in key)):
        return None
    return kind, key


def discover(control, application, bound=None, family=None):
    """Accept only complete metadata, polling the same scope only while pending.

    Every partial response must independently pass the normal metadata guards.
    Rows are never joined across responses, and a stale cursor is not replayed.
    """
    def require(ok, message):
        if not ok:
            raise VmError(message)
    require(isinstance(application, str) and re.fullmatch(r"[A-Za-z0-9._-]{1,128}", application),
            "invalid discovery application")
    arguments = ["actions", "list", "--programmatic", "--json"]
    scope = None
    if bound is not None or family is not None:
        require(bound is not None and len(bound) >= 3 and bound[0] == application,
                "incomplete action discovery scope")
        room, record = bound[1:3]
        try:
            uuid.UUID(room)
        except (ValueError, TypeError, AttributeError) as error:
            raise VmError("invalid discovery room") from error
        require(isinstance(record, str) and 0 < len(record.encode()) <= 512
                and not any(unicodedata.category(c) == "Cc" for c in record),
                "invalid discovery record")
        require(isinstance(family, str) and re.fullmatch(r"[A-Za-z0-9._-]{1,128}", family),
                "invalid discovery semantic family")
        scope = dict(application=application, room=room, record=record, semantic_family=family)
        for flag, value in scope.items():
            arguments.extend(["--" + flag.replace("_", "-"), value])
    for attempt in range(8):
        result = control(list(arguments))
        rows = _validated_rows(result, application, bound, family, scope)
        completion = result.get("completion")
        require(isinstance(completion, dict)
                and type(completion.get("complete")) is bool
                and isinstance(completion.get("incomplete"), list),
                "unobservable action discovery completion")
        incomplete = completion["incomplete"]
        if completion["complete"]:
            require(not incomplete, "contradictory action discovery completion")
            return rows
        require(bool(incomplete) and all(isinstance(item, dict)
                and isinstance(item.get("provider"), str) and item["provider"]
                and isinstance(item.get("reason"), str) for item in incomplete),
                "unobservable action discovery incomplete providers")
        reasons = sorted({item["reason"] for item in incomplete})
        require(reasons == ["pending"],
                "action discovery incomplete: " + ", ".join(reasons))
        if attempt < 7:
            time.sleep(0.05)
    raise VmError("action discovery timed out after 8 pending responses")


def _validated_rows(result, application, bound, family, scope):
    def require(ok, message):
        if not ok:
            raise VmError(message)
    require(isinstance(result, dict) and result.get("query") == ""
            and result.get("scope") == scope, "action discovery scope was not observed")
    rows = result.get("actions")
    require(isinstance(rows, list) and all(isinstance(row, dict) for row in rows)
            and type(result.get("count")) is int and result["count"] == len(rows),
            "unobservable action discovery count")
    require(len(json.dumps(result, separators=(",", ":")).encode()) <= 900 * 1024,
            "action discovery exceeded bounded response")
    if scope is None:
        return [row for row in rows if row.get("provider") == "app-actions"
                and row.get("application") == application]
    app_rows = []
    for row in rows:
        # Native control appends unbound session diagnostics after the scoped
        # provider catalogue. Those rows never become app action candidates.
        if row.get("provider") != "app-actions":
            require(row.get("provider") == "session-diagnostics"
                    and (row.get("id"), row.get("resource"), row.get("operation")) in
                    (("configure@session-diagnostics", "session-diagnostics", "configure"),
                     ("inspect@session-diagnostics", "session-diagnostics", "inspect"))
                    and all(row.get(key) is None for key in
                            ("application", "room", "record", "semantic_identity")),
                    "foreign row in scoped action discovery")
            continue
        app_rows.append(row)
        require(row.get("provider") == "app-actions"
                and tuple(row.get(k) for k in ("application", "room", "record")) == tuple(bound[:3]),
                "foreign row in scoped action discovery")
        identity = semantic_identity(row)
        require(row.get("semantic_identity") is None or identity is not None,
                "malformed action semantic identity")
        require(identity is None or identity[0] == family, "foreign action semantic family")
    return app_rows
