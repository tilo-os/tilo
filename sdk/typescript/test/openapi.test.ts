/**
 * The checks `docs/api/openapi.json` holds the fake API to.
 *
 * `FakeApi` checks every exchange through `Contract`, so the SDK's suite proves
 * the fake speaks the documented API. These tests prove the checker itself
 * refuses what the spec does not describe, so it cannot quietly pass
 * everything.
 */

import { describe, expect, it } from "bun:test";

import type { Recorded } from "./fakeApi.ts";
import { Contract, ContractError } from "./openapiContract.ts";

const contract = new Contract();

const LIST = "/v1/workspaces/11111111-2222-3333-4444-555555555555/resources";
const HAVE = "/v1/workspaces/11111111-2222-3333-4444-555555555555/contents/have";

function call(
  method = "GET",
  path = LIST,
  options: { query?: Record<string, string>; body?: string; contentType?: string } = {},
): Recorded {
  return {
    method,
    path,
    raw: path,
    query: options.query ?? {},
    headers: options.contentType === undefined ? {} : { "content-type": options.contentType },
    body: options.body ?? "",
  };
}

function answer(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const ROW = { path: "notes/Todo", name: "Todo", label: "Todo", kind: "notes.Page" };
const LISTED = { path: "notes", rows: [ROW] };

async function refused(call: Recorded, response: Response, match: RegExp): Promise<void> {
  let caught: unknown;
  try {
    await contract.check(call, response);
  } catch (error) {
    caught = error;
  }
  expect(caught).toBeInstanceOf(ContractError);
  expect((caught as Error).message).toMatch(match);
}

describe("the openapi contract", () => {
  it("passes a documented exchange", async () => {
    await contract.check(call(), answer(LISTED));
  });

  it("refuses a path the spec does not name", async () => {
    await refused(call("GET", "/v1/nothing"), answer({}), /not a route/);
  });

  it("refuses a method the path does not route", async () => {
    await refused(
      call("PATCH", LIST, { body: "{}", contentType: "application/json" }),
      answer({}),
      /declares no PATCH/,
    );
  });

  it("refuses an answer with an undeclared property", async () => {
    const listed = { path: "notes", rows: [{ ...ROW, bogus: 1 }] };
    await refused(call(), answer(listed), /bogus is not a property the spec names/);
  });

  it("refuses an answer short a required property", async () => {
    const short = Object.fromEntries(Object.entries(ROW).filter(([name]) => name !== "label"));
    await refused(call(), answer({ path: "notes", rows: [short] }), /has no `label`/);
  });

  it("refuses a refusal shaped like something else", async () => {
    await refused(call(), answer({ id: "x", name: "x" }, 503), /answer has no `error`/);
  });

  it("refuses a body with an undeclared property", async () => {
    const sent = JSON.stringify({ hashes: [], bogus: true });
    await refused(
      call("POST", HAVE, { body: sent, contentType: "application/json" }),
      answer({ have: [] }),
      /body\.bogus is not a property/,
    );
  });

  it("refuses a query parameter the route does not declare", async () => {
    await refused(call("GET", LIST, { query: { bogus: "1" } }), answer(LISTED), /does not declare/);
  });

  it("refuses an answer short its content type", async () => {
    const plain = new Response("plain", { status: 200, headers: { "content-type": "text/plain" } });
    await refused(call(), plain, /does not declare/);
  });
});
