"""Holds the fake API to `docs/api/openapi.json`.

Every request the SDK sends and every answer `FakeApi` gives has to be one the
spec describes: a method and path the spec routes, a query and body whose
shape matches, a status the operation declares, and a JSON body matching that
status's schema. `FakeApi` checks each exchange through here, so a test that
drives the SDK proves the fake speaks the documented API; the shapes can no
longer drift apart silently.

The schema walk covers what the spec's schemas use: `type`, `properties`,
`required`, `additionalProperties`, `items`, `enum`, `const`, `oneOf`,
`anyOf`, `minimum`, `maximum`, `minLength`, `maxItems`, `minItems`, `pattern`
and `$ref`. A property the schema does not name is refused, so an answer that
grows a field the API does not document fails too.
"""

from __future__ import annotations

import json
import os
import re
from pathlib import Path
from typing import Any
from urllib.parse import unquote

import httpx

#: Where the spec lives beside the tree; `TILO_OPENAPI` names another copy,
#: as the CI image's temporary copy of the SDK does.
SPEC = Path(
    os.environ.get("TILO_OPENAPI")
    or Path(__file__).resolve().parents[3] / "docs" / "api" / "openapi.json"
)


class ContractError(AssertionError):
    """An exchange the spec does not describe."""


class _Spec:
    """The parts of `openapi.json` this check reads."""

    def __init__(self, spec: dict[str, Any]) -> None:
        self.spec = spec
        self.paths: dict[str, dict[str, Any]] = spec["paths"]
        # Longest templates first: a resource path may itself hold "/", so the
        # template that names `actions/{action}` has to be tried before the one
        # that ends at `resources/{path}`.
        self.templates = sorted(
            self.paths,
            key=lambda path: (
                len(_segments_of(path)),
                sum(1 for part in _segments_of(path) if not part.startswith("{")),
                path,
            ),
            reverse=True,
        )

    def resolve(self, node: Any) -> Any:
        while isinstance(node, dict) and "$ref" in node:
            ref = node["$ref"]
            if not ref.startswith("#/"):
                raise ContractError(f"cannot resolve a non-local reference {ref}")
            found: Any = self.spec
            for part in ref.removeprefix("#/").split("/"):
                part = part.replace("~1", "/").replace("~0", "~")
                found = found[part]
            node = found
        return node

    def operation(self, method: str, segments: list[str]) -> tuple[str, dict[str, Any]]:
        """The path template and operation `method` + `segments` names."""
        for template in self.templates:
            bound = _bind(_segments_of(template), segments)
            if bound is None:
                continue
            operation = self.paths[template].get(method.lower())
            if operation is None:
                raise ContractError(
                    f"{method} {_as_path(segments)} matches {template}, which declares no {method}"
                )
            return template, operation
        raise ContractError(f"{method} {_as_path(segments)} is not a route openapi.json names")


def _segments_of(template: str) -> list[str]:
    return [part for part in template.split("/") if part]


def _as_path(segments: list[str]) -> str:
    return "/" + "/".join(segments)


def _bind(template: list[str], segments: list[str]) -> dict[str, str] | None:
    """The parameter values `segments` binds to `template`, or `None`.

    A `{param}` takes one or more segments: a resource path is one parameter
    though its segments stay in the URL, and a name may hold a `/`.
    """
    if not template:
        return {} if not segments else None
    head, rest = template[0], template[1:]
    if head.startswith("{") and head.endswith("}"):
        name = head[1:-1]
        # Try the shortest take that still lets the rest match, so a literal
        # segment after the parameter lands where the template puts it.
        for take in range(1, len(segments) + 1):
            if not rest and take != len(segments):
                continue
            bound = _bind(rest, segments[take:])
            if bound is not None:
                bound[name] = "/".join(segments[:take])
                return bound
        return None
    if not segments or head != segments[0]:
        return None
    return _bind(rest, segments[1:])


class Contract:
    """Checks one exchange against the spec."""

    def __init__(self, spec: _Spec | None = None) -> None:
        self._spec = spec or _Spec(json.loads(SPEC.read_text()))

    # -- what FakeApi calls ----------------------------------------------

    def check(self, call: dict[str, Any], response: httpx.Response) -> None:
        """Raise `ContractError` unless `call` and its `response` are documented."""
        raw = call.get("raw_path") or call["path"]
        raw = raw.split("?")[0]
        segments = [unquote(part) for part in raw.split("/") if part]
        if not segments or segments[0] != "v1":
            # The device flow (`/keys/new`, `/keys/poll`) is outside the
            # versioned API and its spec.
            return
        template, operation = self._spec.operation(call["method"], segments)
        self._request(template, operation, call, segments)
        self._response(operation, call, response)

    def _request(
        self,
        template: str,
        operation: dict[str, Any],
        call: dict[str, Any],
        segments: list[str],
    ) -> None:
        parameters = [self._spec.resolve(p) for p in operation.get("parameters", [])]
        bound = _bind(_segments_of(template), segments) or {}
        errors: list[str] = []
        # A declared parameter the call does not carry. Path ones are bound by
        # the route match itself; a required query one must be in the URL.
        for parameter in parameters:
            where, name = parameter.get("in"), parameter.get("name")
            if parameter.get("required") and where == "path" and name not in bound:
                errors.append(f"the path does not bind {name}")
            if parameter.get("required") and where == "query" and name not in call["query"]:
                errors.append(f"`{name}` is a required query parameter")
            if where == "path" and name in bound:
                self._check(parameter.get("schema", {}), bound[name], f"path.{name}", errors)
        declared_query = {
            p.get("name"): p for p in parameters if p.get("in") == "query"
        }
        seen: set[str] = set()
        for name in call["query"]:
            seen.add(name)
            if name in declared_query:
                continue
            # An object parameter spreads over `name[key]` in the URL.
            owner = name.split("[", 1)[0]
            if owner in declared_query:
                continue
            errors.append(f"the query carries `{name}`, which {template} does not declare")
        for name, parameter in declared_query.items():
            schema = parameter.get("schema", {})
            if name in call["query"]:
                value: Any = call["query"][name]
            elif schema.get("type") == "object":
                value = {
                    key[len(name) + 1 : -1]: found
                    for key, found in call["query"].items()
                    if key.startswith(f"{name}[") and key.endswith("]")
                }
                if not value:
                    continue
            else:
                continue
            self._check(schema, value, f"query.{name}", errors)
        body = call.get("body") or b""
        content_type = (call["headers"].get("content-type") or "").split(";")[0].strip()
        request_body = operation.get("requestBody")
        if request_body is not None:
            request_body = self._spec.resolve(request_body)
            if not body:
                if request_body.get("required"):
                    errors.append("the call has no body, which this route requires")
            else:
                content = request_body.get("content", {})
                schema = _content_schema(content, content_type)
                if schema is None:
                    errors.append(f"the route takes no `{content_type or 'unknown'}` body")
                elif content_type.startswith("application/json") or not content_type:
                    try:
                        sent = json.loads(body)
                    except ValueError:
                        errors.append("the body is not JSON")
                    else:
                        self._check(schema, sent, "body", errors)
        elif body and content_type.startswith("application/json"):
            errors.append("the route takes no body")
        if errors:
            raise ContractError(
                f"{call['method']} {_as_path(segments)}: " + "; ".join(errors)
            )

    def _response(
        self, operation: dict[str, Any], call: dict[str, Any], response: httpx.Response
    ) -> None:
        status = str(response.status_code)
        responses = operation.get("responses", {})
        answered = responses.get(status) or responses.get("default")
        if answered is None:
            raise ContractError(
                f"{call['method']} {call['path']} answered {status}, which it does not declare"
            )
        answered = self._spec.resolve(answered)
        content_type = (response.headers.get("content-type") or "").split(";")[0].strip()
        content = answered.get("content", {})
        if content_type == "application/json":
            schema = _content_schema(content, content_type)
            if schema is None:
                raise ContractError(
                    f"{call['method']} {call['path']} answered {status} JSON, which it does not declare"
                )
            try:
                value = json.loads(response.content)
            except ValueError:
                raise ContractError(
                    f"{call['method']} {call['path']} answered {status} a body that is not JSON"
                ) from None
            errors: list[str] = []
            self._check(schema, value, "answer", errors)
            if errors:
                raise ContractError(
                    f"{call['method']} {call['path']} answered {status}: " + "; ".join(errors)
                )
        elif content:
            # A stream or a file: the status's content type has to be one the
            # spec names, and its body is not JSON to check.
            if _content_schema(content, content_type) is None and "*/*" not in content:
                raise ContractError(
                    f"{call['method']} {call['path']} answered {status} `{content_type}`, which it does not declare"
                )

    # -- the schema walk --------------------------------------------------

    def _check(self, schema: Any, value: Any, at: str, errors: list[str]) -> None:
        schema = self._spec.resolve(schema)
        if schema is True or schema == {}:
            return
        if schema is False:
            errors.append(f"{at} is not allowed")
            return
        if "const" in schema and value != schema["const"]:
            errors.append(f"{at} is {_short(value)}, not {_short(schema['const'])}")
            return
        if "enum" in schema and value not in schema["enum"]:
            errors.append(f"{at} is {_short(value)}, not one of {_short(schema['enum'])}")
            return
        if "oneOf" in schema:
            matches = _matching(self, schema["oneOf"], value, at)
            if len(matches) != 1:
                errors.append(
                    f"{at} matches {len(matches)} of the oneOf branches, not one"
                )
                return
        if "anyOf" in schema:
            if not _matching(self, schema["anyOf"], value, at):
                errors.append(f"{at} matches none of the anyOf branches")
                return
        types = schema.get("type")
        if types is not None:
            names = types if isinstance(types, list) else [types]
            if not any(_is_type(value, name) for name in names):
                errors.append(f"{at} is {_short(value)}, not {names}")
                return
        if isinstance(value, str):
            if "minLength" in schema and len(value) < schema["minLength"]:
                errors.append(f"{at} is shorter than {schema['minLength']}")
            if "pattern" in schema and re.search(schema["pattern"], value) is None:
                errors.append(f"{at} does not match {schema['pattern']}")
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            if "minimum" in schema and value < schema["minimum"]:
                errors.append(f"{at} is below {schema['minimum']}")
            if "maximum" in schema and value > schema["maximum"]:
                errors.append(f"{at} is above {schema['maximum']}")
        if isinstance(value, list):
            if "minItems" in schema and len(value) < schema["minItems"]:
                errors.append(f"{at} holds fewer than {schema['minItems']} items")
            if "maxItems" in schema and len(value) > schema["maxItems"]:
                errors.append(f"{at} holds more than {schema['maxItems']} items")
            items = schema.get("items")
            if items is not None:
                for index, member in enumerate(value):
                    self._check(items, member, f"{at}[{index}]", errors)
        object_shape = (
            "object" in _names(schema.get("type"))
            or "properties" in schema
            or "additionalProperties" in schema
        )
        if isinstance(value, dict) and object_shape:
            properties: dict[str, Any] = schema.get("properties", {})
            for name in schema.get("required", []):
                if name not in value:
                    errors.append(f"{at} has no `{name}`")
            # A bare `{"type": "object"}` is the spec's way of describing a
            # body the app types itself (an action's arguments), so its keys
            # are the app's, not the spec's.
            if (
                schema.get("type") == "object"
                and "properties" not in schema
                and "additionalProperties" not in schema
            ):
                return
            additional = schema.get("additionalProperties", False)
            for name, member in value.items():
                where = f"{at}.{name}"
                if name in properties:
                    self._check(properties[name], member, where, errors)
                elif additional is False:
                    errors.append(f"{where} is not a property the spec names")
                elif additional is not True:
                    self._check(additional, member, where, errors)


def _matching(contract: Contract, branches: list[Any], value: Any, at: str) -> list[int]:
    """The indexes of the `oneOf`/`anyOf` branches `value` matches."""
    return [
        index
        for index, branch in enumerate(branches)
        if not _errors_of(contract, branch, value, at)
    ]


def _errors_of(contract: Contract, schema: Any, value: Any, at: str) -> list[str]:
    errors: list[str] = []
    contract._check(schema, value, at, errors)
    return errors


def _content_schema(content: dict[str, Any], content_type: str) -> Any:
    """The schema `content` gives `content_type`, by name or as `*/*`."""
    if content_type in content and "schema" in content[content_type]:
        return content[content_type]["schema"]
    if "*/*" in content and "schema" in content["*/*"]:
        return content["*/*"]["schema"]
    return None


def _names(types: Any) -> list[str]:
    if types is None:
        return []
    return types if isinstance(types, list) else [types]


def _is_type(value: Any, name: str) -> bool:
    if name == "object":
        return isinstance(value, dict)
    if name == "array":
        return isinstance(value, list)
    if name == "string":
        return isinstance(value, str)
    if name == "boolean":
        return isinstance(value, bool)
    if name == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if name == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if name == "null":
        return value is None
    return True


def _short(value: Any) -> str:
    text = json.dumps(value)
    return text if len(text) <= 120 else text[:117] + "..."
