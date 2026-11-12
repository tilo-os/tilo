/**
 * The wire: MCP over HTTP plus the door's content and key endpoints.
 *
 * Every call is a `tools/call` of an MCP session, exactly as an agent makes
 * it, with `_meta.so.tilo.mcp` asking for typed answers and naming the run
 * the call is a step of. The account is one session (`/mcp`); each workspace
 * is another (`/w/<workspace>/mcp`).
 */

import { AskPending, ProtocolError, RATE_LIMITED, RateLimited, Refused, keyRefusal } from "./errors.ts";

/** The door a client reaches when none is named (`TILO_DOOR` names another). */
export const DEFAULT_DOOR = "https://mcp.tilo.sh";
/** How long a call keeps asking again while the host is busy, in ms. */
export const RETRY_DEADLINE_MS = 60_000;
/** The MCP extension the SDK asks for typed answers through. */
export const SURFACE = "so.tilo.mcp";
/** The header an MCP session's id travels in. */
export const SESSION_HEADER = "mcp-session-id";
/** The SDK's version. */
export const VERSION = "0.1.0";
const PROTOCOL_VERSION = "2025-06-18";

/** The one function a client needs to reach the door: `fetch`, unless you bring your own. */
export type FetchLike = typeof fetch;
/** A typed answer, as the door's MCP tools return it. */
export type Answer = Record<string, unknown>;

/** The door a call goes to: the one named, else `TILO_DOOR`, else the default. */
export function doorFrom(door?: string | null): string {
  const chosen = door ?? process.env.TILO_API_URL ?? DEFAULT_DOOR;
  return chosen.replace(/\/+$/, "");
}

/** The key a call goes with: the one named, else `TILO_API_KEY`. */
export function keyFrom(key?: string | null): string | null {
  return key ?? process.env.TILO_API_KEY ?? null;
}

/** The JSON-RPC messages of a reply: a JSON body, or an event stream. */
export async function messagesIn(response: Response): Promise<any[]> {
  const type = response.headers.get("content-type") ?? "";
  const text = await response.text();
  if (type.includes("text/event-stream")) {
    const messages: any[] = [];
    for (const line of text.split("\n")) {
      if (!line.startsWith("data:")) {
        continue;
      }
      const payload = line.slice("data:".length).trim();
      if (!payload) {
        continue;
      }
      try {
        messages.push(JSON.parse(payload));
      } catch (error) {
        throw new ProtocolError(`the door sent an event that is not JSON: ${String(error)}`);
      }
    }
    return messages;
  }
  if (text.trim() === "") {
    return [];
  }
  try {
    const answer = JSON.parse(text);
    return Array.isArray(answer) ? answer : [answer];
  } catch {
    throw new ProtocolError(
      `the door answered ${response.status} without JSON: ${text.slice(0, 200)}`,
      response.status,
    );
  }
}

function replyIn(messages: any[], requestId: number): any {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message && typeof message === "object" && message.id === requestId) {
      return message;
    }
  }
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message && typeof message === "object" && "id" in message) {
      return message;
    }
  }
  throw new ProtocolError("the door answered without a reply");
}

/** One MCP session: `path` names the account or one workspace. */
export class Session {
  id: string | null = null;
  readonly path: string;
  readonly transport: Transport;
  #ids = 0;

  constructor(transport: Transport, path: string) {
    this.transport = transport;
    this.path = path;
  }

  async open(): Promise<void> {
    const id = (this.#ids += 1);
    const response = await this.#send({
      jsonrpc: "2.0",
      id,
      method: "initialize",
      params: {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: "Tilo SDK", version: VERSION },
      },
    });
    if (response.status >= 400) {
      throw await this.transport.refused(response);
    }
    const opened = replyIn(await messagesIn(response), id);
    if (opened.error) {
      throw refusalOf(opened.error);
    }
    const session = response.headers.get(SESSION_HEADER);
    if (session === null) {
      throw new ProtocolError("the door answered initialize without a session id");
    }
    this.id = session;
    await this.#send({
      jsonrpc: "2.0",
      method: "notifications/initialized",
      params: {},
    });
  }

  async close(): Promise<void> {
    const id = this.id;
    this.id = null;
    if (id === null) {
      return;
    }
    try {
      await this.transport.request("DELETE", this.path, { session: id });
    } catch {
      // A session that is already gone needs no ending of ours.
    }
  }

  /**
   * One `tools/call`, answered typed. A call the host refused because it is
   * busy with the person's changes ran nothing, so it is asked again after
   * the wait the host said, with jitter, until the transport's retry
   * deadline; then `RateLimited`.
   */
  async call(
    tool: string,
    args: Record<string, unknown> = {},
    run: Record<string, unknown> | null = null,
  ): Promise<Answer> {
    const deadline = Date.now() + this.transport.retryDeadlineMs;
    for (let attempt = 0; ; attempt += 1) {
      let busy: RateLimited;
      try {
        const answer = await this.#call(tool, args, run);
        const refused = rateLimited(answer);
        if (refused === null) {
          return answer;
        }
        busy = refused;
      } catch (error) {
        if (!(error instanceof RateLimited)) {
          throw error;
        }
        busy = error;
      }
      const delay =
        Math.max(busy.retryAfterMs ?? 0, Math.min(50 * 2 ** attempt, 2000)) *
        (1 + Math.random() * 0.5);
      if (Date.now() + delay > deadline) {
        throw busy;
      }
      await new Promise((resolve) => setTimeout(resolve, delay));
    }
  }

  async #call(
    tool: string,
    args: Record<string, unknown>,
    run: Record<string, unknown> | null,
  ): Promise<Answer> {
    if (this.id === null) {
      await this.open();
    }
    let id = (this.#ids += 1);
    const payload = (requestId: number) => ({
      jsonrpc: "2.0",
      id: requestId,
      method: "tools/call",
      params: {
        name: tool,
        arguments: args,
        _meta: { [SURFACE]: this.transport.surface(run) },
      },
    });
    let response = await this.#send(payload(id));
    if (response.status === 404) {
      // The door forgot the session (it idles out): open another one.
      this.id = null;
      await this.open();
      id = this.#ids += 1;
      response = await this.#send(payload(id));
    }
    if (response.status >= 400) {
      throw await this.transport.refused(response);
    }
    const reply = replyIn(await messagesIn(response), id);
    if (reply.error) {
      throw refusalOf(reply.error);
    }
    const result = reply.result;
    if (!result || typeof result !== "object") {
      throw new ProtocolError("the door answered a call without a result");
    }
    return this.transport.structured(result as Answer);
  }

  #send(body: unknown): Promise<Response> {
    return this.transport.request("POST", this.path, {
      body: body as Record<string, unknown>,
      session: this.id,
    });
  }
}

/** The door and this client's key: where every session is opened. */
export class Transport {
  readonly door: string;
  readonly key: string | null;
  readonly fetch: FetchLike;
  readonly retryDeadlineMs: number;
  #sessions = new Map<string | null, Session>();

  constructor(
    options: {
      door?: string | null;
      key?: string | null;
      fetch?: FetchLike;
      retryDeadlineMs?: number;
    } = {},
  ) {
    this.door = doorFrom(options.door);
    this.key = keyFrom(options.key);
    this.fetch = options.fetch ?? fetch;
    this.retryDeadlineMs = options.retryDeadlineMs ?? RETRY_DEADLINE_MS;
  }

  /** The session for the account (`null`) or one workspace. */
  session(workspace: string | null = null): Session {
    const known = this.#sessions.get(workspace);
    if (known !== undefined) {
      return known;
    }
    const path = workspace === null ? "/mcp" : `/w/${workspace}/mcp`;
    const session = new Session(this, path);
    this.#sessions.set(workspace, session);
    return session;
  }

  /** @internal */
  async close(): Promise<void> {
    for (const session of [...this.#sessions.values()]) {
      await session.close();
    }
  }

  /** @internal */
  surface(run: Record<string, unknown> | null): Record<string, unknown> {
    const surface: Record<string, unknown> = { structured: true };
    if (run !== null) {
      surface.run = run;
    }
    return surface;
  }

  /**
   * What the door's refusal `response` throws: `KeyRevoked`, `KeyExpired` or
   * `KeyRefused` for a key it turned away, typed as it said; else a
   * `ProtocolError` with its words.
   */
  async refused(response: Response): Promise<Error> {
    if (response.status === 401) {
      try {
        const body = (await response.clone().json()) as Record<string, unknown> | null;
        const typed = keyRefusal(body?.error, response.status);
        if (typed !== null) {
          return typed;
        }
      } catch {
        // Not JSON: the door's words below.
      }
    }
    return new ProtocolError(await this.refusal(response), response.status);
  }

  /** @internal */
  async refusal(response: Response): Promise<string> {
    const text = (await response.text()).trim() || response.statusText;
    if (response.status === 401 || response.status === 403) {
      if (this.key === null) {
        return `no key: pass key or set TILO_API_KEY (${text})`;
      }
      return `this key may not do that: ${text}`;
    }
    if (response.status === 404) {
      return `the door has no such session or workspace: ${text}`;
    }
    return `the door answered ${response.status}: ${text}`;
  }

  /** The typed answer of a tool result, or why there is none. */
  structured(result: Answer): Answer {
    const content = result.structuredContent;
    if (content && typeof content === "object" && !Array.isArray(content)) {
      return content as Answer;
    }
    const text = Array.isArray(result.content)
      ? result.content
          .map((part: Record<string, unknown>) => String(part.text ?? ""))
          .join("")
      : "";
    throw new ProtocolError(
      `the door answered text, not a typed value${text.trim() ? `: ${text.trim().slice(0, 400)}` : ""}`,
    );
  }

  /** @internal */
  async request(
    method: string,
    path: string,
    options: {
      body?: Record<string, unknown>;
      data?: Uint8Array | string;
      session?: string | null;
    } = {},
  ): Promise<Response> {
    const headers: Record<string, string> = {
      accept: "application/json, text/event-stream",
      "user-agent": `tilo-typescript/${VERSION}`,
    };
    if (this.key !== null) {
      headers.authorization = `Bearer ${this.key}`;
    }
    if (options.session != null) {
      headers[SESSION_HEADER] = options.session;
    }
    let body: BodyInit | undefined;
    if (options.body !== undefined) {
      headers["content-type"] = "application/json";
      body = JSON.stringify(options.body);
    } else if (options.data !== undefined) {
      body = options.data as BodyInit;
    }
    const url = new URL(path, this.door).toString();
    try {
      return await this.fetch(url, { method, headers, body });
    } catch (error) {
      throw new ProtocolError(`could not reach the door at ${this.door}: ${String(error)}`);
    }
  }
}

/**
 * A JSON-RPC error as the SDK throws it: `RateLimited` when its data says
 * the host is busy, else a `ProtocolError`.
 */
function refusalOf(error: Record<string, unknown>): Error {
  const message = String(error.message ?? "the door refused the call");
  const data = error.data as Record<string, unknown> | undefined;
  if (data && typeof data === "object" && data.code === RATE_LIMITED) {
    return new RateLimited(message, [], waitOf(data));
  }
  return new ProtocolError(message, null, (error.code as number | string | undefined) ?? null);
}

function waitOf(fields: Record<string, unknown>): number | null {
  const wait = fields.retry_after_ms;
  return typeof wait === "number" && wait >= 0 ? wait : null;
}

/** The `RateLimited` a typed answer is, if it is one. */
export function rateLimited(answer: Answer): RateLimited | null {
  const error = answer.error as Record<string, unknown> | undefined;
  if (answer.status !== "failed" || !error || typeof error !== "object") {
    return null;
  }
  if (error.code !== RATE_LIMITED) {
    return null;
  }
  return new RateLimited(
    String(error.message ?? ""),
    Array.isArray(error.next_steps) ? error.next_steps.map(String) : [],
    waitOf(error),
  );
}

/** The typed answer, or the refusal it is. */
export function answerOf(answer: Answer): Answer {
  if (answer.status === "failed") {
    const busy = rateLimited(answer);
    if (busy !== null) {
      throw busy;
    }
    const error = answer.error;
    const typed = keyRefusal(error, null);
    if (typed !== null) {
      throw typed;
    }
    if (error && typeof error === "object") {
      const detail = error as Record<string, unknown>;
      throw new Refused(
        String(detail.code ?? "failed"),
        String(detail.message ?? ""),
        Array.isArray(detail.next_steps) ? detail.next_steps.map(String) : [],
      );
    }
    throw new Refused("failed", error === undefined || error === null ? "" : String(error));
  }
  return answer;
}

/** The ask an answer names, if it waits for the person. */
export function askOf(
  answer: Answer,
  follow: (invocationId: string) => Promise<Answer>,
): AskPending | null {
  if (answer.status !== "pending") {
    return null;
  }
  const ask = answer.ask;
  if (!ask || typeof ask !== "object") {
    return null;
  }
  const question = ask as Record<string, unknown>;
  const invocationId = String(answer.invocation_id ?? question.invocation_id ?? "");
  const url = textOr(question.url);
  return new AskPending(url, invocationId, question, () => follow(invocationId));
}

function textOr(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

const CONTENT_PREFIX = "tilo.content:";

/**
 * What an action answered: its value, or, when it gave none and made a file
 * instead (Writer's `export-content`), that file's `tilo.content:` reference,
 * which `ws.files.download` fetches. Several files answer a list.
 */
export function resultOf(answer: Answer): unknown {
  if (answer.result !== undefined && answer.result !== null) {
    return answer.result;
  }
  const resources = Array.isArray(answer.resources) ? answer.resources : [];
  const made = resources.filter(
    (resource): resource is string =>
      typeof resource === "string" && resource.startsWith(CONTENT_PREFIX),
  );
  if (made.length === 1) {
    return made[0];
  }
  return made.length > 0 ? made : null;
}
