"""The checks `docs/api/openapi.json` holds the fake API to.

`FakeApi` checks every exchange through `Contract`, so the SDK's suite proves
the fake speaks the documented API. These tests prove the checker itself
refuses what the spec does not describe, so it cannot quietly pass
everything.
"""

from __future__ import annotations

import json
from typing import Any

import httpx
import pytest

from openapi_contract import Contract, ContractError


@pytest.fixture(scope="module")
def contract() -> Contract:
    return Contract()


def call(
    method: str = "GET",
    path: str = "/v1/workspaces/11111111-2222-3333-4444-555555555555/webhooks",
    *,
    query: dict[str, str] | None = None,
    body: bytes = b"",
    content_type: str | None = None,
) -> dict[str, Any]:
    headers = {} if content_type is None else {"content-type": content_type}
    return {
        "method": method,
        "path": path,
        "raw_path": path,
        "query": query or {},
        "headers": headers,
        "body": body,
    }


def answer(body: Any, status: int = 200, content_type: str = "application/json") -> httpx.Response:
    return httpx.Response(
        status,
        content=json.dumps(body).encode(),
        headers={"content-type": content_type},
    )


HOOKS = "/v1/workspaces/11111111-2222-3333-4444-555555555555/webhooks"
LISTED = {"webhooks": [{"id": "h1", "url": "https://example.test/hook", "events": []}]}


def test_a_documented_exchange_passes(contract: Contract) -> None:
    contract.check(call(), answer(LISTED))


def test_a_path_the_spec_does_not_name_is_refused(contract: Contract) -> None:
    with pytest.raises(ContractError, match="not a route"):
        contract.check(call(path="/v1/nothing"), answer({}))


def test_a_method_the_path_does_not_route_is_refused(contract: Contract) -> None:
    with pytest.raises(ContractError, match="declares no PATCH"):
        contract.check(
            call("PATCH", HOOKS, body=b"{}", content_type="application/json"),
            answer({}),
        )


def test_an_answer_with_an_undeclared_property_is_refused(contract: Contract) -> None:
    listed = {"webhooks": [{"id": "x", "url": "https://x", "events": [], "bogus": 1}]}
    with pytest.raises(ContractError, match="bogus is not a property the spec names"):
        contract.check(call(), answer(listed))


def test_an_answer_short_a_required_property_is_refused(contract: Contract) -> None:
    with pytest.raises(ContractError, match="has no `url`"):
        contract.check(call(), answer({"webhooks": [{"id": "x", "events": []}]}))


def test_a_refusal_shaped_like_something_else_is_refused(contract: Contract) -> None:
    # `default` is the route's refusal, whatever the status: an answer there
    # has to be the error shape.
    with pytest.raises(ContractError, match="answer has no `error`"):
        contract.check(call(), answer({"id": "x", "name": "x"}, 503))


def test_a_body_with_an_undeclared_property_is_refused(contract: Contract) -> None:
    sent = b'{"url": "https://x", "bogus": true}'
    # A 201 answer, as the route declares, so only the request is at fault.
    with pytest.raises(ContractError, match=r"body\.bogus is not a property"):
        contract.check(
            call("POST", HOOKS, body=sent, content_type="application/json"),
            answer({"id": "x", "url": "https://x", "events": []}, 201),
        )


def test_a_query_parameter_the_route_does_not_declare_is_refused(contract: Contract) -> None:
    with pytest.raises(ContractError, match="does not declare"):
        contract.check(call(query={"bogus": "1"}), answer(LISTED))


def test_an_answer_short_its_content_type_is_refused(contract: Contract) -> None:
    plain = httpx.Response(200, content=b"plain", headers={"content-type": "text/plain"})
    with pytest.raises(ContractError, match="does not declare"):
        contract.check(call(), plain)
