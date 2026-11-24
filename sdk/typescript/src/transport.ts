/**
 * What carries a tool call to Tilo: this computer's daemon, or a key's
 * hosted server. Every SDK call is one tool of Tilo's agent surface
 * (`tilo_ls`, `tilo_read`, `tilo_do`, …); a transport answers its typed result,
 * or throws the refusal.
 *
 * - [`DaemonTransport`], the default: the native addon, connecting to this
 *   computer's daemon as the person or as an app the launcher started.
 * - [`KeyTransport`]: a key, reaching the hosted server's MCP endpoint.
 *
 * A program may bring its own transport: anything that answers `workspace()`
 * and `call()`.
 */

import {
  KeyRefused,
  ProtocolError,
  Refused,
  Unreachable,
  keyRefusalOf,
  refusalOf,
} from "./errors.ts";
import { recordOf, textOf } from "./models.ts";

/** The SDK's version, as the hosted server reads it in the user agent. */
export const VERSION = "0.1.0";
/** The addon this computer's daemon is reached through. */
export const NATIVE_PACKAGE = "tilo-sdk-native";
/** How long an action is waited for before it is followed by its id, in ms. */
export const ACTION_WAIT_MS = 10_000;
/** The longest one `tilo_watch` holds before it answers with what it has. */
export const WATCH_WAIT_MS = 15_000;

/** The invocation group a call is a step of: what History lists it under. */
export interface Group {
  id: string;
  title: string;
}

/**
 * One way to reach Tilo's tools. `workspace` is the workspace calls act in
 * when they name none, which is how an app finds its one workspace; `call`
 * makes one tool call, in `workspace` (`null`: no workspace named, which is
 * the account's own tree), and answers its typed result.
 */
export interface Transport {
  workspace(): Promise<string | null>;
  call(
    workspace: string | null,
    tool: string,
    args: Record<string, unknown>,
    group: Group | null,
  ): Promise<Record<string, unknown>>;
  /**
   * Make the web call `args` names with the key of the vault item it names, in `workspace`. An
   * app's held connection has the vault add the key to the request; without the daemon's
   * transport, a call is refused.
   */
  webCall?(workspace: string | null, args: Record<string, unknown>): Promise<Record<string, unknown>>;
  /**
   * The secret of the vault item `item` in `workspace`, released to this
   * app's process. Only the daemon's transport carries one; without it a
   * release is refused.
   */
  release?(workspace: string | null, item: string): Promise<string>;
  /**
   * The reference of the vault item tagged for `app` and `purpose` in
   * `workspace`, or `null`. Only the daemon's transport carries a find;
   * without it a find is refused.
   */
  find?(workspace: string | null, app: string, purpose: string): Promise<string | null>;
  /**
   * The call lane of an app's held connection: carry an outside call into a workspace call (or
   * none), say who is in it, hand over what its people say, and take what the workspace's call
   * says. Only the daemon's transport carries one, on a connection the daemon handed an app.
   */
  carryCall?(pair: CarriedCall | null): Promise<void>;
  reportCall?(report: CallReport): Promise<void>;
  hearCall?(frame: CallFrame): void;
  takeCall?(): Promise<CallTaken>;
}

/** An outside service's call and the workspace call it is carried into, as one call. */
export interface CarriedCall {
  /** The service's id of its call (a voice channel there). */
  outside: string;
  /** The workspace's voice channel it is one call with. */
  tilo: string;
}

/** One person in an outside call, as the service shows them. */
export interface CallPerson {
  /** Their account on the service. */
  account: string;
  /** The name the service shows for them. */
  name: string;
  /** Where the service serves their picture. */
  avatar_url?: string | null;
  /** BLAKE3 hex of their picture, once it is kept in the workspace's content. */
  avatar?: string | null;
  muted?: boolean;
  deafened?: boolean;
}

/** Where the app stands in the outside call it carries. */
export type CallLink =
  | { link: "idle" }
  | { link: "joining"; call: string }
  | { link: "joined"; call: string }
  | { link: "refused"; call: string; reason: string };

/** Who is in each outside call, and where the app stands in them. */
export interface CallReport {
  calls: { call: string; people: CallPerson[] }[];
  link: CallLink;
  /** The service's own line about its call; empty leaves it as it was. */
  status?: string;
}

/** One 20 ms Opus frame of a call (48 kHz), untouched, and the account that said it. */
export interface CallFrame {
  speaker: string;
  opus: Uint8Array;
}

/**
 * Where the device asks the app to be in the outside call: in `call`, showing `status` to its
 * people about who is on the workspace's side, or in none (`call: null`).
 */
export interface CallAsk {
  call: string | null;
  status: string | null;
}

/** What a carried call said since the app last looked, and a new ask, if there is one. */
export interface CallTaken {
  frames: CallFrame[];
  asked: CallAsk | null;
}

/** `fetch`, unless you bring your own. */
export type FetchLike = typeof fetch;

/** The addon's daemon, as `native/index.d.ts` declares it. */
interface NativeDaemon {
  readonly workspace: string | null;
  call(
    workspace: string | null,
    tool: string,
    args: Record<string, unknown>,
    group: Group | null,
  ): Promise<unknown>;
  release(workspace: string | null, item: string): Promise<unknown>;
  webCall(workspace: string | null, args: Record<string, unknown>): Promise<unknown>;
  find(workspace: string | null, app: string, purpose: string): Promise<unknown>;
  carryCall(pair: CarriedCall | null): Promise<unknown>;
  reportCall(report: CallReport): Promise<unknown>;
  hearCall(speaker: string, opus: Uint8Array): unknown;
  saidCall(): {
    frames: { speaker: string; opus: Uint8Array }[];
    asked?: { call?: string | null; status?: string | null } | null;
  };
}

interface NativeModule {
  connect(socket: string | null, workspace: string | null): Promise<NativeDaemon>;
}

/** The addon, loaded the first time a call needs it; named when it is missing. */
async function nativeAddon(): Promise<NativeModule> {
  try {
    return (await import(NATIVE_PACKAGE)) as NativeModule;
  } catch (error) {
    throw new ProtocolError(
      `this client reaches this computer's daemon through the ${NATIVE_PACKAGE} addon, which is ` +
        `not installed: install it with \`bun add ${NATIVE_PACKAGE}\`, or give the client a key ` +
        `(\`new Tilo({ key })\`) to reach the hosted server (${String(error)})`,
    );
  }
}

/**
 * Tilo's tools as this computer's daemon answers them, over the native
 * addon's action frames. The connection is made the first time a call needs
 * it, so constructing a client never fails; `socket` and `workspace` are what
 * the addon's `connect` takes, both usually `null`.
 */
export class DaemonTransport implements Transport {
  readonly socket: string | null;
  readonly named: string | null;
  #daemon: Promise<NativeDaemon> | null = null;
  /** The daemon once connected, for what a live call hands it without waiting. */
  #ready: NativeDaemon | null = null;

  constructor(socket: string | null = null, workspace: string | null = null) {
    this.socket = socket;
    this.named = workspace;
  }

  /** The daemon, connected the first time and again after a lost connection. */
  #opened(): Promise<NativeDaemon> {
    if (this.#daemon === null) {
      this.#daemon = nativeAddon()
        .then((module) => module.connect(this.socket, this.named))
        .then((daemon) => {
          this.#ready = daemon;
          return daemon;
        })
        .catch((error: unknown) => {
          this.#daemon = null;
          this.#ready = null;
          throw error;
        });
    }
    return this.#daemon;
  }

  /** The workspace the daemon acts in when a call names none. */
  async workspace(): Promise<string | null> {
    return (await this.#opened()).workspace ?? this.named;
  }

  /** One tool call as action frames to the daemon: its typed answer, or `Refused`. */
  async call(
    workspace: string | null,
    tool: string,
    args: Record<string, unknown>,
    group: Group | null,
  ): Promise<Record<string, unknown>> {
    const daemon = await this.#opened();
    let answer: unknown;
    try {
      answer = await daemon.call(workspace, tool, args, group);
    } catch (error) {
      // The addon answers a refusal as `{refused}`; a throw is the connection.
      throw new Unreachable(error instanceof Error ? error.message : String(error));
    }
    const fields = recordOf(answer);
    if ("answer" in fields) {
      return recordOf(fields.answer);
    }
    if ("refused" in fields) {
      throw refusalOf(fields.refused);
    }
    throw new ProtocolError(`the daemon answered ${tool} in a shape the SDK does not know`);
  }

  /**
   * Make the web call `args` names with the key of the vault item it names: one action frame to
   * the vault, answered for the process this connection is. An app's held connection is asked
   * for its person's grant once per service and item; a plain program's call goes through
   * `tilo_do`, as every program's does.
   */
  async webCall(
    workspace: string | null,
    args: Record<string, unknown>,
  ): Promise<Record<string, unknown>> {
    const daemon = await this.#opened();
    let answer: unknown;
    try {
      answer = await daemon.webCall(workspace, args);
    } catch (error) {
      throw new Unreachable(error instanceof Error ? error.message : String(error));
    }
    const fields = recordOf(answer);
    if ("answer" in fields) {
      return recordOf(fields.answer);
    }
    if ("refused" in fields) {
      throw refusalOf(fields.refused);
    }
    throw new ProtocolError("the daemon answered a web call in a shape the SDK does not know");
  }

  /**
   * The secret of the vault item `item`, released to this app's process: one
   * action frame to the vault, not a tool. The vault releases a secret only to
   * the process an app's connection was handed to, for an item its grant
   * covers, and refuses every agent. The secret is answered to the caller
   * alone and kept nowhere.
   */
  async release(workspace: string | null, item: string): Promise<string> {
    const daemon = await this.#opened();
    let answer: unknown;
    try {
      answer = await daemon.release(workspace, item);
    } catch (error) {
      throw new Unreachable(error instanceof Error ? error.message : String(error));
    }
    const fields = recordOf(answer);
    if ("refused" in fields) {
      throw refusalOf(fields.refused);
    }
    const secret = recordOf(fields.answer).secret;
    if (typeof secret !== "string") {
      throw new ProtocolError("the vault answered a release with no secret");
    }
    return secret;
  }

  /**
   * The reference of the vault item tagged for `app` and `purpose`, found on this app's own
   * connection, or `null` when the vault keeps none. The find releases nothing, so the person is
   * not asked; a release of the item afterwards is.
   */
  async find(workspace: string | null, app: string, purpose: string): Promise<string | null> {
    const daemon = await this.#opened();
    let answer: unknown;
    try {
      answer = await daemon.find(workspace, app, purpose);
    } catch (error) {
      throw new Unreachable(error instanceof Error ? error.message : String(error));
    }
    const fields = recordOf(answer);
    if ("refused" in fields) {
      throw refusalOf(fields.refused);
    }
    const reference = recordOf(fields.answer).reference;
    if (reference === undefined || reference === null) {
      return null;
    }
    if (typeof reference !== "string") {
      throw new ProtocolError("the vault answered a find with no reference");
    }
    return reference;
  }

  /** Carry `pair` on this app's connection, or let go of the call carried (`null`). */
  async carryCall(pair: CarriedCall | null): Promise<void> {
    const daemon = await this.#opened();
    let answer: unknown;
    try {
      answer = await daemon.carryCall(pair);
    } catch (error) {
      throw new Unreachable(error instanceof Error ? error.message : String(error));
    }
    laneAnswered(answer, "a carried call");
  }

  /** Say who is in the outside calls this app's connection carries, and where it stands. */
  async reportCall(report: CallReport): Promise<void> {
    const daemon = await this.#opened();
    let answer: unknown;
    try {
      answer = await daemon.reportCall(report);
    } catch (error) {
      throw new Unreachable(error instanceof Error ? error.message : String(error));
    }
    laneAnswered(answer, "a call report");
  }

  /**
   * Hand the workspace's call one frame an outside person said. Nothing answers it: a frame the
   * call does not take is dropped there, as live audio is, and so is one said before the
   * connection is open.
   */
  hearCall(frame: CallFrame): void {
    const daemon = this.#ready;
    if (daemon === null) {
      void this.#opened().catch(() => {});
      return;
    }
    const answer = daemon.hearCall(frame.speaker, frame.opus);
    if (answer !== null && answer !== undefined) {
      const fields = recordOf(answer);
      if ("refused" in fields) {
        throw refusalOf(fields.refused);
      }
    }
  }

  /** What the workspace's call said since the last time, and a new ask, if there is one. */
  async takeCall(): Promise<CallTaken> {
    const daemon = await this.#opened();
    let said: ReturnType<NativeDaemon["saidCall"]>;
    try {
      said = daemon.saidCall();
    } catch (error) {
      // The addon throws a refusal's body as its message.
      const message = error instanceof Error ? error.message : String(error);
      let body: unknown;
      try {
        body = { error: JSON.parse(message).error ?? JSON.parse(message) };
      } catch {
        throw new Unreachable(message);
      }
      throw refusalOf(body);
    }
    const asked = said.asked ?? null;
    return {
      frames: said.frames.map((frame) => ({ speaker: frame.speaker, opus: frame.opus })),
      asked: asked === null ? null : { call: asked.call ?? null, status: asked.status ?? null },
    };
  }
}

/** A lane call's answer: nothing, or its refusal raised. */
function laneAnswered(answer: unknown, what: string): void {
  const fields = recordOf(answer);
  if ("refused" in fields) {
    throw refusalOf(fields.refused);
  }
  if (!("answer" in fields)) {
    throw new ProtocolError(`the daemon answered ${what} in a shape the SDK does not know`);
  }
}

/** The MCP protocol version this client opens a session with. */
const PROTOCOL_VERSION = "2025-06-18";
/** The most a failed call waits before asking again. */
const BACKOFF_CAP_MS = 2_000;

/**
 * Tilo's tools as a hosted server answers them, with a key, as MCP
 * `tools/call` over HTTPS. This is the only HTTP the SDK speaks, and it is
 * not `/v1`.
 */
export class KeyTransport implements Transport {
  readonly url: string;
  readonly key: string;
  readonly retryDeadlineMs: number;
  readonly fetch: FetchLike;
  readonly signal: AbortSignal | null;
  #session: string | null = null;
  #opened = false;
  #nextId = 1;

  constructor(
    url: string,
    key: string,
    options: { fetch?: FetchLike; retryDeadlineMs?: number; signal?: AbortSignal } = {},
  ) {
    this.url = url.replace(/\/+$/, "");
    this.key = key;
    this.retryDeadlineMs = options.retryDeadlineMs ?? 60_000;
    this.fetch = options.fetch ?? globalThis.fetch;
    this.signal = options.signal ?? null;
  }

  /** A key names no one workspace: `tilo.workspace()` without a ref throws. */
  async workspace(): Promise<string | null> {
    return null;
  }

  /** One tool call to the server's MCP endpoint as the key: its typed answer, or `Refused`. */
  async call(
    workspace: string | null,
    tool: string,
    args: Record<string, unknown>,
    group: Group | null,
  ): Promise<Record<string, unknown>> {
    const arguments_: Record<string, unknown> = { ...args };
    if (workspace !== null) {
      arguments_.workspace = workspace;
    }
    const meta: Record<string, unknown> = { structured: true };
    if (group !== null) {
      meta.invocation = group;
    }
    const request = {
      jsonrpc: "2.0",
      id: this.#nextId++,
      method: "tools/call",
      params: {
        name: tool,
        arguments: arguments_,
        _meta: { "so.tilo.mcp": meta },
      },
    };
    const deadline =
      this.retryDeadlineMs > 0 ? Date.now() + this.retryDeadlineMs : Number.NEGATIVE_INFINITY;
    let reopened = false;
    for (let attempt = 0; ; attempt += 1) {
      let response: Response;
      try {
        await this.#open();
        response = await this.#post(request);
      } catch (error) {
        if (!(error instanceof Unreachable) || (this.signal?.aborted ?? false) || Date.now() >= deadline) {
          throw error;
        }
        await pause(delayOf(attempt, null));
        continue;
      }
      // A session the server forgot is opened again, and the call asked once more.
      if (response.status === 404 && this.#session !== null) {
        await discard(response);
        if (reopened) {
          throw new ProtocolError("the server lost this session, twice, and the call did not run");
        }
        reopened = true;
        this.#session = null;
        this.#opened = false;
        continue;
      }
      if (response.status === 401 || response.status === 403) {
        throw await this.#keyRefusal(response);
      }
      if (response.status === 503) {
        const refusal = refusalOrNull(await jsonOf(response));
        const wait =
          retryAfterOf(response.headers) ??
          (refusal instanceof Refused ? refusal.retryAfterMs : null);
        if (Date.now() + (wait ?? 0) > deadline) {
          throw refusal ?? new Refused("unavailable", "the server is briefly unavailable");
        }
        await pause(delayOf(attempt, wait));
        continue;
      }
      if (response.status >= 400) {
        const refusal = refusalOrNull(await jsonOf(response));
        if (refusal !== null) {
          throw refusal;
        }
        throw new ProtocolError(`the server answered ${response.status}`);
      }
      const answer = answerOf(await jsonOf(response), tool);
      if (answer instanceof Refused && (answer.code === "not_ready" || answer.code === "unanswered")) {
        if (Date.now() + (answer.retryAfterMs ?? 0) > deadline) {
          throw answer;
        }
        await pause(delayOf(attempt, answer.retryAfterMs));
        continue;
      }
      if (answer instanceof Error) {
        throw answer;
      }
      return answer;
    }
  }

  /** Open the MCP session once: its id goes on every later request. */
  async #open(): Promise<void> {
    if (this.#opened) {
      return;
    }
    const response = await this.#post({
      jsonrpc: "2.0",
      id: 0,
      method: "initialize",
      params: {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: "tilo-sdk", version: VERSION },
      },
    });
    if (response.status === 401 || response.status === 403) {
      throw await this.#keyRefusal(response);
    }
    if (response.status >= 400) {
      await discard(response);
      throw new ProtocolError(`the server answered ${response.status} to initialize`);
    }
    const session = response.headers.get("mcp-session-id");
    await discard(response);
    this.#session = session !== null && session !== "" ? session : null;
    this.#opened = true;
  }

  /** One request to `${url}/mcp`, its answer as it came. */
  async #post(body: unknown): Promise<Response> {
    const headers: Record<string, string> = {
      authorization: `Bearer ${this.key}`,
      "content-type": "application/json",
      accept: "application/json",
      "user-agent": `tilo-typescript/${VERSION}`,
    };
    if (this.#session !== null) {
      headers["mcp-session-id"] = this.#session;
    }
    try {
      return await this.fetch(`${this.url}/mcp`, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
        signal: this.signal ?? undefined,
      });
    } catch (error) {
      throw new Unreachable(`could not reach the server at ${this.url}: ${String(error)}`);
    }
  }

  /** The key refusal a 401 or 403 is: its body's `error`, else its `www-authenticate`. */
  async #keyRefusal(response: Response): Promise<KeyRefused> {
    let detail: Record<string, unknown> = {};
    try {
      detail = recordOf(recordOf(await jsonOf(response)).error);
    } catch {
      // A body that is not a refusal's JSON is read as none: the header says why.
      detail = {};
    }
    if (typeof detail.code === "string") {
      return keyRefusalOf(detail, `the server refused the key (${response.status})`);
    }
    const header = response.headers.get("www-authenticate") ?? "";
    const code = attributeOf(header, "error");
    const message =
      attributeOf(header, "error_description") ?? `the server refused the key (${response.status})`;
    return keyRefusalOf({ code, message }, message);
  }
}

/** The answer a `tools/call` reply carries, or the refusal it was. */
function answerOf(body: unknown, tool: string): Record<string, unknown> | Error {
  const envelope = recordOf(body);
  if (envelope.error !== undefined && envelope.error !== null) {
    const error = recordOf(envelope.error);
    return new Refused(
      "failed",
      typeof error.message === "string" ? error.message : "the call failed",
    );
  }
  const result = recordOf(envelope.result);
  const structured = result.structuredContent;
  const failed = result.isError === true;
  if (structured !== undefined && structured !== null && (!failed || tool === "tilo")) {
    return recordOf(structured);
  }
  if (structured !== undefined && structured !== null) {
    const typed = recordOf(structured);
    const error = recordOf(typed.error);
    return refusalOf({
      error: {
        ...error,
        action: typed.action ?? error.action,
      },
    });
  }
  const content = Array.isArray(result.content) ? result.content : [];
  const first = recordOf(content[0]);
  return new Refused(
    failed ? "failed" : "not_found",
    textOf(first.text) ?? "the call failed",
  );
}

/** A refusal from a body the server answered with a status, when it has one. */
function refusalOrNull(body: unknown): Error | null {
  const fields = recordOf(body);
  return fields.error === undefined ? null : refusalOf(fields);
}

async function jsonOf(response: Response): Promise<unknown> {
  let text: string;
  try {
    text = await response.text();
  } catch (error) {
    throw new Unreachable(`the server's answer was cut short: ${String(error)}`);
  }
  const trimmed = text.trim();
  if (trimmed === "") {
    return null;
  }
  try {
    return JSON.parse(trimmed);
  } catch {
    throw new ProtocolError(`the server answered something that is not JSON: ${trimmed.slice(0, 200)}`);
  }
}

async function discard(response: Response): Promise<void> {
  try {
    await response.body?.cancel();
  } catch {
    // Nothing was read; there is nothing to release.
  }
}

/** How long the server says to wait, from its `Retry-After` header, in ms. */
function retryAfterOf(headers: Headers): number | null {
  const value = headers.get("retry-after");
  if (value === null) {
    return null;
  }
  const seconds = Number(value);
  return Number.isFinite(seconds) && seconds >= 0 ? seconds * 1000 : null;
}

/** How long to wait before asking again: 50 ms doubling to 2 s, or what the server asked. */
function delayOf(attempt: number, wait: number | null): number {
  return Math.max(wait ?? 0, Math.min(50 * 2 ** attempt, BACKOFF_CAP_MS));
}

/** One attribute of a `www-authenticate` header (`error="invalid_token"`). */
function attributeOf(header: string, name: string): string | null {
  const found = new RegExp(`(?:^|[,\\s])${name}="([^"]*)"`).exec(header);
  return found?.[1] ?? null;
}

function pause(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
