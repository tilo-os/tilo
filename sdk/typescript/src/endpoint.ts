/**
 * One endpoint the client calls: the account, or one workspace.
 *
 * Every call goes through here to the transport, with the key's headers, the
 * run it is a step of, an `Idempotency-Key` when it changes something, and
 * the API's version checked; a refusal or an event stream comes back as its
 * typed value.
 */

import { AsyncLocalStorage } from "node:async_hooks";

import {
  AskPending,
  ProtocolError,
  Refused,
  Unreachable,
  UnsupportedVersion,
  refusalOf,
} from "./errors.ts";
import {
  actionAnswerOf,
  type ActionAnswer,
  type Event,
  eventOf,
  type ProgramAnswer,
  programAnswerOf,
  resultOf,
} from "./models.ts";
import { API_VERSION, query as queryString, routes, splitAction } from "./routes.ts";
import { frames } from "./sse.ts";
import { Invocation } from "./invocation.ts";
import {
  collect,
  type HeadersLike,
  type Transport,
  type TransportReply,
} from "./transport.ts";

/** The API server a client reaches when none is named. */
export const DEFAULT_URL = "https://api.tilo.sh";
/** How long a call keeps asking again while the host is busy, in ms. */
export const RETRY_DEADLINE_MS = 60_000;
/** The SDK's version. */
export const VERSION = "0.1.0";
/** The five seconds the API holds an action for, at most a minute. */
const MAX_WAIT_SECONDS = 60;
const MUTATING = new Set(["POST", "PUT", "PATCH", "DELETE"]);

const storage = new AsyncLocalStorage<InvocationGroup>();

/**
 * An invocation group a set of calls is attributed to: `Tilo-Invocation`
 * names it on every call and `Tilo-Invocation-Title` its title, so the apps
 * record one undo step and history names each of them.
 *
 * `links` are the links to every record its calls changed, each once, in the
 * order first changed; `link` is the one link when they changed one record,
 * else `null`: a group across several records hands over `links`.
 */
export class InvocationGroup {
  readonly id: string;
  readonly title: string;
  readonly workspace: string;
  readonly invocations: string[] = [];
  readonly links: string[] = [];
  private readonly endpoint: Endpoint;

  constructor(endpoint: Endpoint, title: string, id: string) {
    this.endpoint = endpoint;
    this.workspace = endpoint.id;
    this.title = title;
    this.id = id;
  }

  /**
   * Take back exactly what its calls changed, in every record. The
   * host refuses, changing nothing, when someone else's change stands on top
   * of the group's in a record.
   */
  async undo(): Promise<ActionAnswer> {
    const last = this.invocations.at(-1);
    if (last === undefined) {
      throw new ProtocolError("the invocation group made no call to take back");
    }
    return this.endpoint.undo(last);
  }

  /** @internal */
  note(answer: ActionAnswer): void {
    if (answer.invocation !== null && !this.invocations.includes(answer.invocation)) {
      this.invocations.push(answer.invocation);
    }
    for (const link of linksOf(answer)) {
      if (!this.links.includes(link)) {
        this.links.push(link);
      }
    }
  }

  /** @internal The headers a call of this group carries. */
  headers(): Record<string, string> {
    return { "tilo-invocation": this.id, "tilo-invocation-title": this.title };
  }

  /** The link to the one record it changed, or `null`. */
  get link(): string | null {
    return this.links.length === 1 ? (this.links[0] ?? null) : null;
  }
}

/** The record links an answer carries: a call's `link`, a batch's `links`. */
export function linksOf(answer: ActionAnswer): string[] {
  const found = [...answer.links];
  if (answer.link !== null) {
    found.unshift(answer.link);
  }
  return found.filter((link) => link !== "");
}

/** Run `body` with `group` the invocation group every call inside is a step of. */
export function withinInvocation<T>(group: InvocationGroup, body: () => Promise<T>): Promise<T> {
  return storage.run(group, body);
}

/** The invocation group the current call is a step of, when one is open. */
export function currentGroup(): InvocationGroup | null {
  return storage.getStore() ?? null;
}

/** What one request may carry besides its method and path. */
export interface RequestOptions {
  /** A JSON body, as the API takes it. */
  json?: unknown;
  /** Bytes, as they are: an upload's file. */
  data?: Uint8Array | ArrayBuffer | string;
  headers?: Record<string, string>;
  /** The query's fields; `null`, `undefined` and `""` are left out. */
  query?: Record<string, string | number | boolean | null | undefined>;
  /** The body is wanted as it arrives: an event stream, or a file. */
  stream?: boolean;
  /** Send an `Idempotency-Key`; on for POST, PUT, PATCH and DELETE. */
  idempotent?: boolean;
  /** The `Idempotency-Key` to send, in place of a fresh one per call. */
  idempotencyKey?: string;
  signal?: AbortSignal;
}

/** What an answer was: its status, its headers, its JSON body or its stream. */
export interface Reply {
  status: number;
  headers: HeadersLike;
  body: Record<string, unknown> | null;
  chunks: AsyncIterable<Uint8Array> | null;
}

/** The account's calls, or one workspace's. */
export class Endpoint {
  readonly transport: Transport;
  readonly workspace: string | null;
  readonly retryDeadlineMs: number;
  /** Ends the streams this endpoint opened, when its client closes. */
  readonly signal: AbortSignal | null;

  constructor(
    transport: Transport,
    workspace: string | null = null,
    retryDeadlineMs = RETRY_DEADLINE_MS,
    signal: AbortSignal | null = null,
  ) {
    this.transport = transport;
    this.workspace = workspace;
    this.retryDeadlineMs = retryDeadlineMs;
    this.signal = signal;
  }

  /** The workspace's id, which a workspace route needs. */
  get id(): string {
    if (this.workspace === null) {
      throw new ProtocolError("this call names one workspace, and this endpoint is the account's");
    }
    return this.workspace;
  }

  /**
   * One request, its refusal raised: a call the host is busy with is asked
   * again with the same `Idempotency-Key` until the retry deadline, as is one
   * that never arrived.
   */
  async request(method: string, path: string, options: RequestOptions = {}): Promise<Reply> {
    const headers: Record<string, string> = {
      accept: options.stream ? "text/event-stream, application/octet-stream" : "application/json",
      "user-agent": `tilo-typescript/${VERSION}`,
      ...options.headers,
    };
    const group = storage.getStore();
    if (group !== undefined && this.workspace !== null && group.workspace === this.workspace) {
      Object.assign(headers, group.headers());
    }
    const idempotencyKey =
      options.idempotencyKey ??
      ((options.idempotent ?? MUTATING.has(method)) ? crypto.randomUUID() : null);
    if (idempotencyKey !== null) {
      headers["idempotency-key"] = idempotencyKey;
    }
    const url = path + queryString(options.query ?? {});
    const deadline = Date.now() + this.retryDeadlineMs;
    for (let attempt = 0; ; attempt += 1) {
      let reply: TransportReply;
      try {
        reply = await this.transport.request(method, url, {
          json: options.json,
          data: options.data,
          headers,
          stream: options.stream,
          signal: options.signal,
        });
      } catch (error) {
        if (!(error instanceof Unreachable) || Date.now() >= deadline) {
          throw error;
        }
        await pause(delayOf(attempt, null));
        continue;
      }
      versionOf(reply.headers);
      if (reply.status !== 503) {
        return await shaped(reply, options.stream === true);
      }
      const refusal = refusalOf(reply.status, await jsonOf(reply));
      if (!(refusal instanceof Refused)) {
        throw refusal;
      }
      const wait = retryAfterOf(reply.headers);
      if (Date.now() + (wait ?? 0) > deadline) {
        throw refusal;
      }
      await pause(delayOf(attempt, wait));
    }
  }

  /**
   * One request's JSON answer; a refusal of it is thrown as what it was
   * (`Refused`, `KeyRevoked`, `SurfaceChanged`), and a refusal of a server
   * that is briefly unavailable is asked again.
   */
  async answer(method: string, path: string, options: RequestOptions = {}): Promise<Record<string, unknown>> {
    const reply = await this.request(method, path, options);
    if (reply.status >= 400) {
      throw refusalOf(reply.status, reply.body);
    }
    const body = reply.body ?? {};
    const group = storage.getStore();
    if (group !== undefined && group.workspace === this.workspace) {
      group.note(actionAnswerOf(body));
    }
    return body;
  }

  /** What `GET /v1` answers: the API's version, and what the key reaches. */
  info(): Promise<Record<string, unknown>> {
    return this.answer("GET", routes.info(), { idempotent: false });
  }

  /**
   * One action, its arguments the body: its result, the `Invocation` that follows
   * it, or `AskPending` when it waits for the person. `surface` is the app's
   * version of it, for a call made through generated types.
   */
  async action(
    path: string,
    args: Record<string, unknown> = {},
    surface: string | null = null,
  ): Promise<unknown> {
    const { resource, action } = splitAction(path);
    const headers: Record<string, string> = {};
    if (surface !== null) {
      headers["tilo-surface"] = surface;
    }
    const answer = actionAnswerOf(
      await this.answer("POST", routes.action(this.id, resource, action), { json: args, headers }),
    );
    if (answer.status === "pending") {
      if (answer.ask !== null) {
        throw this.askOf(answer);
      }
      return new Invocation(this, answer.invocation ?? "", answer.path ?? path);
    }
    return resultOf(answer);
  }

  /**
   * Several actions of the resource at `path` as one: `{actions: […]}` to its
   * `group` action, answering that call's answer. A call the host has not
   * settled is followed to its answer; one that waits for the person throws
   * the ask.
   */
  async group(
    path: string,
    members: Array<{ action: string; args: Record<string, unknown> }>,
    surface: string | null,
  ): Promise<ActionAnswer> {
    const headers: Record<string, string> = {};
    if (surface !== null) {
      headers["tilo-surface"] = surface;
    }
    const answer = actionAnswerOf(
      await this.answer("POST", routes.action(this.id, path, "group"), {
        json: { actions: members },
        headers,
      }),
    );
    const group = storage.getStore();
    if (group !== undefined && this.workspace !== null && group.workspace === this.workspace) {
      group.note(answer);
    }
    if (answer.status === "pending") {
      if (answer.ask !== null) {
        throw this.askOf(answer);
      }
      return this.wait(answer.invocation ?? "");
    }
    return answer;
  }

  /** The `AskPending` an answer that waits for the person is. */
  askOf(answer: ActionAnswer): AskPending {
    const run = answer.invocation ?? "";
    const url = typeof answer.ask?.url === "string" ? answer.ask.url : null;
    return new AskPending(url, run, answer.ask, async () => {
      const settled = await this.wait(run);
      const result = resultOf(settled);
      return result !== null && typeof result === "object" ? (result as Record<string, unknown>) : {};
    });
  }

  /** Follow an invocation until it is no longer pending, its whole answer. */
  async wait(invocation: string, timeoutMs: number | null = null): Promise<ActionAnswer> {
    const deadline = timeoutMs === null ? null : Date.now() + timeoutMs;
    for (;;) {
      const seconds = deadline === null
        ? MAX_WAIT_SECONDS
        : Math.max(0, Math.min(MAX_WAIT_SECONDS, Math.ceil((deadline - Date.now()) / 1000)));
      const answer = actionAnswerOf(
        await this.answer("GET", routes.invocation(this.id, invocation), {
          query: { wait: seconds },
          idempotent: false,
        }),
      );
      if (answer.status !== "pending") {
        return answer;
      }
      if (deadline !== null && Date.now() >= deadline) {
        throw new ProtocolError(`the invocation ${invocation} is still running`);
      }
    }
  }

  /** The invocation as it stands now, without waiting. */
  async invocation(invocation: string): Promise<ActionAnswer> {
    return actionAnswerOf(
      await this.answer("GET", routes.invocation(this.id, invocation), {
        query: { wait: 0 },
        idempotent: false,
      }),
    );
  }

  /** Stop an invocation. */
  async cancel(invocation: string): Promise<ActionAnswer> {
    return actionAnswerOf(await this.answer("POST", routes.invocationCancel(this.id, invocation)));
  }

  /** Take back what an invocation changed: the one call, or every call of its group. */
  async undo(invocation: string): Promise<ActionAnswer> {
    return actionAnswerOf(await this.answer("POST", routes.invocationUndo(this.id, invocation)));
  }

  /**
   * Run a tilolang program as one step, in the program session `session` names (an earlier
   * answer's `session`), or a new one.
   */
  async program(
    source: string,
    confirm: boolean | null = null,
    session: string | null = null,
  ): Promise<ProgramAnswer> {
    const json: Record<string, unknown> = { program: source };
    if (confirm !== null) {
      json.confirm = confirm;
    }
    if (session !== null) {
      json.session = session;
    }
    return programAnswerOf(await this.answer("POST", routes.programs(this.id), { json }));
  }

  /**
   * An invocation's events, from started to succeeded, failed or cancelled.
   * The stream ends with it.
   */
  async *output(invocation: string): AsyncGenerator<Event> {
    yield* this.stream(routes.invocationOutput(this.id, invocation), null, null);
  }

  /**
   * The workspace's events as they happen, `path`'s alone or everything under
   * it: each change, each invocation starting or finishing, and who is there.
   * The
   * cursor is kept, so a stream that ends is read on from where it stopped.
   */
  async *events(path: string | null = null): AsyncGenerator<Event> {
    let cursor: string | null = null;
    while (this.signal === null || !this.signal.aborted) {
      try {
        for await (const event of this.stream(routes.events(this.id), cursor, path)) {
          if (event.cursor !== null && event.cursor !== "") {
            cursor = event.cursor;
          }
          yield event;
        }
      } catch (error) {
        if (!(error instanceof Unreachable) || (this.signal?.aborted ?? false)) {
          return;
        }
      }
      await pause(1000);
    }
  }

  /**
   * One server-sent event stream: `under` is the resource path it follows
   * (the workspace's changes alone), `resume` where a reconnect reads on.
   */
  private async *stream(
    route: string,
    resume: string | null,
    under: string | null,
  ): AsyncGenerator<Event> {
    const headers: Record<string, string> = {};
    if (resume !== null) {
      headers["last-event-id"] = resume;
    }
    const reply = await this.request("GET", route, {
      headers,
      query: { path: under },
      stream: true,
      idempotent: false,
      signal: this.signal ?? undefined,
    });
    if (reply.chunks === null) {
      if (reply.status >= 400) {
        throw refusalOf(reply.status, reply.body);
      }
      throw new ProtocolError(`the API answered ${reply.status} with no event stream`, reply.status);
    }
    for await (const frame of frames(reply.chunks)) {
      if (frame.event === "error") {
        throw refusalOf(500, { error: parseJson(frame.data) });
      }
      yield eventOf(frame.event, parseJson(frame.data), frame.id);
    }
  }
}

/** The typed answer of a transport's reply, or what it answered instead. */
async function shaped(reply: TransportReply, stream: boolean): Promise<Reply> {
  if (stream && reply.status < 400) {
    const chunks = reply.body === null ? null : chunksOfBody(reply.body);
    return { status: reply.status, headers: reply.headers, body: null, chunks };
  }
  const body = parseJson(new TextDecoder().decode(await collect(reply.body)));
  return { status: reply.status, headers: reply.headers, body, chunks: null };
}

async function* chunksOfBody(body: AsyncIterable<Uint8Array> | Uint8Array): AsyncGenerator<Uint8Array> {
  if (body instanceof Uint8Array) {
    yield body;
    return;
  }
  yield* body;
}

/** A body as JSON, or `null` when it is empty; other bodies are the API breaking. */
function parseJson(text: string): Record<string, unknown> | null {
  const trimmed = text.trim();
  if (trimmed === "") {
    return null;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    throw new ProtocolError(`the API answered something that is not JSON: ${trimmed.slice(0, 200)}`);
  }
  return parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)
    ? (parsed as Record<string, unknown>)
    : null;
}

async function jsonOf(reply: TransportReply): Promise<Record<string, unknown> | null> {
  return parseJson(new TextDecoder().decode(await collect(reply.body)));
}

/** The API's major version, which every answer names: another one is refused. */
function versionOf(headers: HeadersLike): void {
  const version = headers.get("tilo-api-version");
  if (version === null) {
    return;
  }
  const major = version.split(".")[0]?.trim() ?? version.trim();
  if (major !== API_VERSION) {
    throw new UnsupportedVersion(version, API_VERSION);
  }
}

/** How long the API says to wait, from its `Retry-After` header, in ms. */
function retryAfterOf(headers: HeadersLike): number | null {
  const value = headers.get("retry-after");
  if (value === null) {
    return null;
  }
  const seconds = Number(value);
  return Number.isFinite(seconds) && seconds >= 0 ? seconds * 1000 : null;
}

/** How long to wait before asking again: what the server said, else growing with jitter. */
function delayOf(attempt: number, wait: number | null): number {
  const growing = Math.min(50 * 2 ** attempt, 2000);
  return Math.max(wait ?? 0, growing) * (1 + Math.random() * 0.5);
}

function pause(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
