/** The boundary under every call: the request, its retries, and what an answer means. */

import { describe, expect, it } from "bun:test";

import {
  Endpoint,
  HttpTransport,
  NATIVE_PACKAGE,
  ProtocolError,
  Refused,
  Tilo,
  Unreachable,
  UnsupportedVersion,
} from "../src/index.ts";
import { FakeApi } from "./fakeApi.ts";

/** A read and an action of the account, as every resource and action is reached. */
const USAGE = "/v1/resources/account/usage";
const CREATE = "/v1/resources/account/workspaces/actions/create";

function open(api: FakeApi, retryDeadlineMs = 500): Endpoint {
  return new Endpoint(
    new HttpTransport("https://api.test", api.key, api.fetch),
    "11111111-2222-3333-4444-555555555555",
    retryDeadlineMs,
  );
}

describe("a request", () => {
  it("sends the key, the accept header and the user agent", async () => {
    const api = new FakeApi();
    await open(api).answer("GET", USAGE, { idempotent: false });

    const call = api.last();
    expect(call?.method).toBe("GET");
    expect(call?.path).toBe(USAGE);
    expect(call?.headers.authorization).toBe(`Bearer ${api.key}`);
    expect(call?.headers.accept).toBe("application/json");
    expect(call?.headers["user-agent"]).toMatch(/^tilo-typescript\//);
  });

  it("sends an Idempotency-Key with every mutating call, the same one on a retry", async () => {
    const api = new FakeApi();
    api.busy = 1;
    await open(api).answer("POST", CREATE, { json: { name: "w" } });

    const sent = api.asked("POST", CREATE);
    expect(sent).toHaveLength(2);
    const first = sent[0]?.headers["idempotency-key"] ?? "";
    expect(first).not.toBe("");
    expect(sent[1]?.headers["idempotency-key"]).toBe(first);
    expect(sent[1]?.body).toBe('{"name":"w"}');
  });

  it("sends a new key for each logical call", async () => {
    const api = new FakeApi();
    const endpoint = open(api);
    await endpoint.answer("POST", CREATE, { json: { name: "a" } });
    await endpoint.answer("POST", CREATE, { json: { name: "b" } });

    const sent = api.asked("POST", CREATE);
    expect(sent[0]?.headers["idempotency-key"]).not.toBe(sent[1]?.headers["idempotency-key"]);
  });

  it("leaves the key off a read", async () => {
    const api = new FakeApi();
    await open(api).answer("GET", USAGE, { idempotent: false });
    expect(api.last()?.headers["idempotency-key"]).toBeUndefined();
  });

  it("asks again while the server is unavailable, then throws its refusal past the deadline", async () => {
    const api = new FakeApi();
    api.busy = 50;

    const failure = open(api, 100).answer("POST", CREATE, { json: { name: "w" } });
    await expect(failure).rejects.toMatchObject({ code: "unavailable" });
    expect(api.asked("POST", CREATE).length).toBeGreaterThan(1);
  });

  it("asks again when the call never arrived", async () => {
    let attempts = 0;
    const api = new FakeApi();
    const answer = (input: string, init?: RequestInit) => {
      attempts += 1;
      if (attempts === 1) {
        throw new TypeError("fetch failed");
      }
      return api.handle(input, init);
    };
    const endpoint = new Endpoint(new HttpTransport("https://api.test", api.key, answer as typeof fetch), null);

    const read = await endpoint.answer("GET", USAGE, { idempotent: false });
    expect(attempts).toBe(2);
    expect(read.path).toBe("account/usage");
  });

  it("refuses an answer of another API major version", async () => {
    const api = new FakeApi();
    api.version = "2";

    const failure = open(api).answer("GET", "/v1", { idempotent: false });
    await expect(failure).rejects.toBeInstanceOf(UnsupportedVersion);
    const error = await failure.catch((caught: unknown) => caught);
    expect(error).toBeInstanceOf(ProtocolError);
    expect(error).toMatchObject({
      name: "UnsupportedVersion",
      code: "api_version_unsupported",
      status: null,
      version: "2",
      speaking: "1",
      message: "this SDK speaks Tilo API 1; the server answers 2: update tilo-sdk",
    });
  });

  it("rejects with Unreachable when the endpoint cannot be reached", async () => {
    const failing = (async () => {
      throw new TypeError("fetch failed");
    }) as unknown as typeof fetch;
    const endpoint = new Endpoint(new HttpTransport("https://api.test", "tilo_sk_test", failing), null, 0);

    const failure = endpoint.answer("GET", USAGE, { idempotent: false });
    await expect(failure).rejects.toBeInstanceOf(Unreachable);
    const error = await failure.catch((caught: unknown) => caught);
    expect(error).toBeInstanceOf(ProtocolError);
    expect(error).toMatchObject({
      name: "Unreachable",
    });
  });

  it("throws a refusal as it was, and an answer that is not JSON as ProtocolError", async () => {
    const api = new FakeApi();
    const endpoint = open(api);

    await expect(
      endpoint.answer(
        "GET",
        "/v1/workspaces/11111111-2222-3333-4444-555555555555/resources/notes/Nowhere",
        { idempotent: false },
      ),
    ).rejects.toBeInstanceOf(Refused);

    const text = new HttpTransport("https://api.test", api.key, (async () =>
      new Response("not json", { status: 200 })) as unknown as typeof fetch);
    await expect(
      new Endpoint(text, null).answer("GET", "/v1", { idempotent: false }),
    ).rejects.toBeInstanceOf(ProtocolError);
  });
});

describe("a host in process", () => {
  it("names the addon to install when it is absent", async () => {
    const opening = Tilo.host("https://eu1.tilo.sh", { keyFile: "/home/ada/.local/state/tilo/key" });
    await expect(opening).rejects.toBeInstanceOf(ProtocolError);
    await expect(
      Tilo.host("https://eu1.tilo.sh", { keyFile: "/home/ada/.local/state/tilo/key" }),
    ).rejects.toThrow(NATIVE_PACKAGE);
  });
});
