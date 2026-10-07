"""Bounded metadata discovery over the existing semantic action door."""
import json
import re
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
    """Bootstrap by metadata; once bound, require the server's exact scope."""
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
    result = control(arguments)
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
    for row in rows:
        require(row.get("provider") == "app-actions"
                and tuple(row.get(k) for k in ("application", "room", "record")) == tuple(bound[:3]),
                "foreign row in scoped action discovery")
        identity = semantic_identity(row)
        require(row.get("semantic_identity") is None or identity is not None,
                "malformed action semantic identity")
        require(identity is None or identity[0] == family, "foreign action semantic family")
    return rows
