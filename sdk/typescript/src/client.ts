/**
 * The client: Tilo's API, `/v1`, as the generated operations (`types/api-v1.ts`) describe it,
 * carried by this computer's daemon or by a key. What is written here is how a request goes out
 * and comes back, what names an invocation group, and the two things an operation alone does
 * not do: following an action to its end, and putting a file up in pieces.
 */

import { AsyncLocalStorage } from "node:async_hooks";

import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";

import { Refused, refusalOf } from "./errors.ts";
import { resourceProxy, type Resource, type Surface } from "./resource.ts";
import { Daemon, type DaemonConnection, type FetchLike, keySend, type Send } from "./transport.ts";
import {
  AccountOperations,
  type ActionAnswer,
  type Event,
  type Operation,
  WorkspaceOperations,
} from "./types/api-v1.ts";
import { CallLane, Vault } from "./vault.ts";

/** The server a key reaches when none is named. */
export const DEFAULT_URL = "https://api.tilo.sh";
/** How long a request keeps asking again while the API is briefly unavailable, in ms. */
export const RETRY_DEADLINE_MS = 60_000;
/** The most one request of an upload carries; a larger file goes up in pieces. */
const PIECE = 16 * 1024 * 1024;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** How a client is made: how it reaches Tilo, and how long it asks again. */
export interface TiloOptions {
  /** A key: `TILO_API_KEY` when not given. With one, requests go by URL. */
  key?: string | null;
  /** Where a key's requests go: `TILO_API_URL` when not given, else `https://api.tilo.sh`. */
  url?: string | null;
  /** The daemon's native socket: this computer's daemon when not given. */
  socket?: string | null;
  /** The workspace an app's connection acts in, when the daemon serves more than one. */
  workspace?: string | null;
  /** A daemon connection of your own instead of the addon's. */
  daemon?: DaemonConnection;
  /** How a key's requests are made: `fetch` unless you bring your own. */
  fetch?: FetchLike;
  /** How long a request keeps asking again while the API answers `503`, in ms (0: never). */
  retryDeadlineMs?: number;
}

/** Where a key's requests go: the URL named, else `TILO_API_URL`, else the default. */
export function urlFrom(url?: string | null): string {
  return (url ?? process.env.TILO_API_URL ?? DEFAULT_URL).replace(/\/+$/, "");
}

/** The key requests go with: the one named, else `TILO_API_KEY`. */
export function keyFrom(key?: string | null): string | null {
  return key ?? process.env.TILO_API_KEY ?? null;
}

const groups = new AsyncLocalStorage<InvocationGroup>();

/** @internal The invocation group the current call is made in, when one is open. */
export function currentGroup(): InvocationGroup | null {
  return groups.getStore() ?? null;
}

/**
 * An invocation group: every request made inside `ws.invocation(title, …)` names it in
 * `Tilo-Invocation` and `Tilo-Invocation-Title`, so the apps record one undo step and History
 * shows the title. `links` are the records its calls changed, each once; `link` is the one when
 * they changed one.
 */
export class InvocationGroup {
  /** What every request of the group names it by. */
  readonly id: string;
  /** What History calls it. */
  readonly title: string;
  /** The workspace its calls are in. */
  readonly workspace: Workspace;
  /** The invocations its calls answered, in order. */
  readonly invocations: string[] = [];
  /** The links to the records its calls changed, each once. */
  readonly links: string[] = [];

  constructor(workspace: Workspace, title: string, id: string) {
    this.workspace = workspace;
    this.title = title;
    this.id = id;
  }

  /** The link to the one record its calls changed, or `null`. */
  get link(): string | null {
    return this.links.length === 1 ? (this.links[0] ?? null) : null;
  }

  /** Take back what its calls changed, in every record. */
  async undo(): Promise<ActionAnswer> {
    const last = this.invocations.at(-1);
    if (last === undefined) {
      throw new Refused({ code: "not_found", message: "the invocation group made no call to take back" });
    }
    return this.workspace.undoInvocation({ invocation: last });
  }

  /** @internal What one of its calls answered. */
  note(answer: unknown): void {
    const said = (answer ?? {}) as Partial<ActionAnswer>;
    if (typeof said.invocation === "string" && !this.invocations.includes(said.invocation)) {
      this.invocations.push(said.invocation);
    }
    for (const link of [said.link, ...(said.links ?? [])]) {
      if (typeof link === "string" && link !== "" && !this.links.includes(link)) {
        this.links.push(link);
      }
    }
  }
}

/**
 * The account, and where every request goes: `new Tilo()` reaches this computer's daemon through
 * the `tilo-sdk-native` addon, `new Tilo({ key })` reaches `/v1` by URL. Its methods are the
 * API's account operations; `workspace(…)` names one workspace.
 */
export class Tilo extends AccountOperations {
  /** This computer's daemon, unless the client goes by key. */
  readonly daemon: Daemon | null;
  /** How long a request keeps asking again while the API answers `503`, in ms. */
  readonly retryDeadlineMs: number;
  readonly #send: Send;
  readonly #streams = new AbortController();

  constructor(options: TiloOptions = {}) {
    super();
    this.retryDeadlineMs = options.retryDeadlineMs ?? RETRY_DEADLINE_MS;
    const key = keyFrom(options.key);
    const own = options.daemon;
    this.daemon =
      own !== undefined
        ? new Daemon(async () => own)
        : key === null
          ? Daemon.native(options.socket ?? null, options.workspace ?? null)
          : null;
    this.#send =
      this.daemon?.send ??
      keySend(urlFrom(options.url), key ?? "", options.fetch ?? globalThis.fetch, this.#streams.signal);
  }

  /** One workspace, by its id or its name; with none, the one the daemon's connection acts in. */
  workspace(ref: string | null = null): Workspace {
    return new Workspace(this, ref);
  }

  /** End the requests and streams of a key's client still open. */
  close(): void {
    this.#streams.abort();
  }

  /** @internal */
  protected send(operation: Operation, params: Record<string, string | undefined>, body: unknown, headers: Record<string, string>): Promise<unknown> {
    return this.request(operation, params, body, headers).then((response) => answerOf(response, operation, null));
  }

  /** @internal */
  protected stream(operation: Operation, params: Record<string, string | undefined>, body: unknown, headers: Record<string, string>): AsyncIterableIterator<Event> {
    return streamOf(async () => ({ response: await this.request(operation, params, body, headers), workspace: "" }));
  }

  /**
   * @internal One request for `operation`: its answer, or the refusal it was thrown. A request
   * names the invocation group it is made in, and a mutating one an `Idempotency-Key`, so asking
   * again while the API is briefly unavailable runs it once.
   */
  async request(
    operation: Operation,
    params: Record<string, string | undefined>,
    body: unknown,
    headers: Record<string, string>,
  ): Promise<Response> {
    const sent = new Map<string, string>();
    sent.set("accept", { json: "application/json", events: "text/event-stream", bytes: "*/*" }[operation.answer]);
    const group = groups.getStore();
    if (group !== undefined) {
      sent.set("tilo-invocation", group.id);
      sent.set("tilo-invocation-title", group.title);
    }
    if (operation.mutating) {
      sent.set("idempotency-key", crypto.randomUUID());
    }
    let bytes: Uint8Array | null = null;
    if (operation.body === "json") {
      bytes = new TextEncoder().encode(JSON.stringify(body ?? {}));
      sent.set("content-type", "application/json");
    } else if (operation.body === "bytes") {
      bytes = body as Uint8Array;
      sent.set("content-type", "application/octet-stream");
    }
    for (const [name, value] of Object.entries(headers)) {
      sent.set(name.toLowerCase(), value);
    }
    const target = targetOf(operation, params);
    const deadline = Date.now() + this.retryDeadlineMs;
    for (let attempt = 0; ; attempt += 1) {
      const response = await this.#send(operation.method, target, [...sent], bytes);
      if (response.ok) {
        return response;
      }
      const refusal = await refusalOfResponse(response);
      const asked = Number(response.headers.get("retry-after") ?? "0") * 1000;
      const wait = Math.max(asked, refusal.error.retry_after_ms ?? 0, Math.min(50 * 2 ** attempt, 2_000));
      if (response.status !== 503 || Date.now() + wait > deadline) {
        throw refusal;
      }
      await new Promise((resolve) => setTimeout(resolve, wait));
    }
  }
}

/**
 * One workspace, named by its id or its name; named by neither, the one the daemon's connection
 * acts in. Its methods are the API's workspace operations, each naming this workspace.
 */
export class Workspace extends WorkspaceOperations {
  /** The client its requests go through. */
  readonly client: Tilo;
  /** What it was named by: an id, a name, or `null` for the daemon's own. */
  readonly ref: string | null;
  /** The person's vault, as this workspace's calls reach it. */
  readonly vault: Vault;
  /** The call lane of an app's own connection. */
  readonly callLane: CallLane;
  #id: Promise<string> | null = null;

  constructor(client: Tilo, ref: string | null) {
    super();
    this.client = client;
    this.ref = ref;
    this.vault = new Vault(this);
    this.callLane = new CallLane(this);
  }

  /** Its id: the one it was named by, the daemon's own, or the one the account lists by its name. */
  id(): Promise<string> {
    this.#id ??= this.#resolve().catch((error: unknown) => {
      this.#id = null;
      throw error;
    });
    return this.#id;
  }

  async #resolve(): Promise<string> {
    if (this.ref !== null && UUID.test(this.ref)) {
      return this.ref;
    }
    const served = (await this.client.daemon?.opened())?.workspace ?? null;
    if (this.ref === null || this.ref === served) {
      if (served === null) {
        throw new Refused({ code: "invalid_request", message: "name a workspace: a key's client serves none" });
      }
      return served;
    }
    const listed = await run(this.client, { path: "account/workspaces", action: "list" });
    const named = (listed.result as { workspaces?: { id: string; name: string }[] } | undefined)?.workspaces;
    const found = named?.find((workspace) => workspace.name === this.ref);
    if (found === undefined) {
      throw new Refused({
        code: "not_found",
        message: `no workspace ${this.ref}`,
        next_steps: ["account/workspaces#list names them"],
      });
    }
    return found.id;
  }

  /** @internal */
  protected async send(operation: Operation, params: Record<string, string | undefined>, body: unknown, headers: Record<string, string>): Promise<unknown> {
    const response = await this.client.request(operation, { ...params, workspace: await this.id() }, body, headers);
    const group = groups.getStore();
    return answerOf(response, operation, group?.workspace === this ? group : null);
  }

  /** @internal */
  protected stream(operation: Operation, params: Record<string, string | undefined>, body: unknown, headers: Record<string, string>): AsyncIterableIterator<Event> {
    return streamOf(async () => {
      const workspace = await this.id();
      return { response: await this.client.request(operation, { ...params, workspace }, body, headers), workspace };
    });
  }

  /**
   * A resource as a typed object: its fields, its actions and its edits. `surface` is the app's
   * version generated types were made from, which every call through it names.
   */
  resource(path: string, surface: Surface = null): Resource {
    return resourceProxy(this, path, surface);
  }

  /**
   * Run `body` as one invocation group: every call inside names it, so the apps record one undo
   * step and History shows `title`.
   */
  invocation<T>(title: string, body: (group: InvocationGroup) => Promise<T>): Promise<T> {
    const slug = title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);
    const group = new InvocationGroup(this, title, `sdk:${slug || "invocation"}:${crypto.randomUUID().slice(0, 8)}`);
    return groups.run(group, () => body(group));
  }

  /**
   * Put a file's bytes into the workspace: the `tilo.content:` reference an action takes it by.
   * The bytes are addressed by their BLAKE3 hash, so a file goes up once whatever it is called;
   * one larger than a request carries goes up in pieces, each continuing where the put stands.
   */
  async upload(data: Uint8Array | ArrayBuffer | Blob): Promise<string> {
    const bytes =
      data instanceof Uint8Array ? data : new Uint8Array(data instanceof ArrayBuffer ? data : await data.arrayBuffer());
    const hash = bytesToHex(blake3(bytes));
    let offset = 0;
    for (;;) {
      const end = Math.min(offset + PIECE, bytes.length);
      const range: Record<string, string> =
        offset === 0 && end === bytes.length ? {} : { "content-range": `bytes ${offset}-${end - 1}/${bytes.length}` };
      const put = await this.putContent({ hash }, bytes.subarray(offset, end), range);
      if (put.content !== undefined && put.content !== null) {
        return put.content;
      }
      if (put.offset <= offset) {
        throw new Refused({ code: "upload_failed", message: `the put stands at ${put.offset} after a piece from ${offset}` });
      }
      offset = put.offset;
    }
  }

  /** The bytes a `tilo.content:` reference names. */
  download(reference: string): Promise<Uint8Array> {
    return this.getContent({ hash: reference.replace(/^tilo\.content:/, "") });
  }
}

/**
 * Run `action` on the resource `path` until it is no longer pending: its answer, `result` what
 * the action produced. A refusal throws. In a workspace a pending action is followed by its
 * invocation; the account's is asked again with the same `Idempotency-Key`, which asks about the
 * same invocation.
 */
export async function run(
  where: Tilo | Workspace,
  params: { path: string; action: string },
  args: Record<string, unknown> = {},
  headers: Record<string, string> = {},
): Promise<ActionAnswer> {
  if (where instanceof Workspace) {
    let answer = await where.runAction(params, args, headers);
    while (answer.status === "pending" && typeof answer.invocation === "string") {
      answer = await where.getInvocation({ invocation: answer.invocation, wait: "60" });
    }
    return answer;
  }
  const again: Record<string, string> = { prefer: "wait=60", "idempotency-key": crypto.randomUUID() };
  for (const [name, value] of Object.entries(headers)) {
    again[name.toLowerCase()] = value;
  }
  for (;;) {
    const answer = await where.runAccountAction(params, args, again);
    if (answer.status !== "pending") {
      return answer;
    }
  }
}

/** `operation`'s path with `params` in its braces, each segment percent-encoded, and its query. */
function targetOf(operation: Operation, params: Record<string, string | undefined>): string {
  const path = operation.path.replace(/\{(\w+)\}/g, (_, name: string) => {
    const value = params[name];
    if (value === undefined) {
      throw new TypeError(`${operation.path} names {${name}}, and the call gave none`);
    }
    return value.split("/").map(encodeURIComponent).join("/");
  });
  const query = new URLSearchParams();
  for (const name of operation.query) {
    const value = params[name];
    if (value !== undefined) {
      query.set(name, value);
    }
  }
  const said = query.toString();
  return said === "" ? path : `${path}?${said}`;
}

/** What a successful answer carries, as `operation` answers it. */
async function answerOf(response: Response, operation: Operation, group: InvocationGroup | null): Promise<unknown> {
  if (operation.answer === "bytes") {
    return new Uint8Array(await response.arrayBuffer());
  }
  const text = await response.text();
  const answer: unknown = text === "" ? {} : JSON.parse(text);
  group?.note(answer);
  return answer;
}

/** The refusal an answer that is not a success is: its body's, else its status. */
async function refusalOfResponse(response: Response): Promise<Refused> {
  const text = await response.text().catch(() => "");
  try {
    const body = JSON.parse(text) as { error?: unknown };
    if (body.error !== undefined) {
      return refusalOf(body, response.status);
    }
  } catch {
    // Not the API's refusal: its status says what it was.
  }
  return new Refused(
    { code: response.status === 503 ? "unavailable" : "failed", message: `the API answered ${response.status}: ${text.slice(0, 200)}` },
    response.status,
  );
}

/**
 * The server-sent events of an answer `open` makes, each an `Event`, the cursor it resumes after
 * as its `id`. An `error` event throws its refusal. `return()` ends it at once, even while it
 * waits on a quiet stream.
 */
function streamOf(open: () => Promise<{ response: Response; workspace: string }>): AsyncIterableIterator<Event> {
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let workspace = "";
  let ended = false;
  let buffer = "";
  const queue: Event[] = [];
  const decoder = new TextDecoder();
  const iterator: AsyncIterableIterator<Event> = {
    async next() {
      while (queue.length === 0 && !ended) {
        if (reader === null) {
          const opened = await open();
          workspace = opened.workspace;
          reader = opened.response.body?.getReader() ?? null;
          if (reader === null || ended) {
            ended = true;
            await reader?.cancel().catch(() => {});
            break;
          }
        }
        const { value, done } = await reader.read();
        if (done) {
          ended = true;
          break;
        }
        buffer += decoder.decode(value, { stream: true });
        for (let at = buffer.indexOf("\n\n"); at >= 0; at = buffer.indexOf("\n\n")) {
          const event = eventOf(buffer.slice(0, at), workspace);
          buffer = buffer.slice(at + 2);
          if (event !== null) {
            queue.push(event);
          }
        }
      }
      const value = queue.shift();
      return value === undefined ? { done: true, value: undefined } : { done: false, value };
    },
    async return() {
      ended = true;
      queue.length = 0;
      await reader?.cancel().catch(() => {});
      return { done: true, value: undefined };
    },
    [Symbol.asyncIterator]() {
      return iterator;
    },
  };
  return iterator;
}

/** One server-sent event's frame as an `Event`; `null` for a comment that keeps it alive. */
function eventOf(frame: string, workspace: string): Event | null {
  let id = "";
  let type = "message";
  const data: string[] = [];
  for (const line of frame.split(/\r?\n/)) {
    if (line === "" || line.startsWith(":")) {
      continue;
    }
    const at = line.indexOf(":");
    const field = at < 0 ? line : line.slice(0, at);
    const value = at < 0 ? "" : line.slice(at + 1).replace(/^ /, "");
    if (field === "id") {
      id = value;
    } else if (field === "event") {
      type = value;
    } else if (field === "data") {
      data.push(value);
    }
  }
  if (data.length === 0) {
    return null;
  }
  const parsed: unknown = JSON.parse(data.join("\n"));
  if (type === "error") {
    throw refusalOf(parsed);
  }
  return { id, type, workspace, data: parsed };
}
