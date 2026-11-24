/**
 * One endpoint the client calls: the account, or one workspace.
 *
 * Every call is one tool of Tilo's agent surface, carried by the client's
 * [`Transport`](transport.ts): the daemon's, or a key's hosted server. An answer
 * comes back as its typed value — a listing, a resource, an `Invocation`, an
 * `AskPending` — and a refusal is thrown.
 */

import { AsyncLocalStorage } from "node:async_hooks";

import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex } from "@noble/hashes/utils.js";

import {
  ACTION_WAIT_MS,
  WATCH_WAIT_MS,
  type Transport,
  type Group,
} from "./transport.ts";
import { AskPending, ProtocolError, SurfaceChanged, Unreachable } from "./errors.ts";
import {
  actionAnswerOf,
  type ActionAnswer,
  arrayOf,
  type Event,
  eventOf,
  type ProgramAnswer,
  programAnswerOf,
  recordOf,
  resultOf,
  textOf,
  type View,
} from "./models.ts";
import { Invocation } from "./invocation.ts";

/** The four phases an invocation ends in, as a watch tells them. */
const ENDED = new Set(["succeeded", "failed", "cancelled", "refused"]);
/** How long the first reconnection of a watch waits, and the most it waits. */
const RECONNECT_MS = 1_000;

const storage = new AsyncLocalStorage<InvocationGroup>();

/**
 * An invocation group a set of calls is attributed to: the group goes with
 * every call of it, so the apps record one undo step and history names each
 * of them.
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
    const found = answer.link !== null ? [answer.link, ...answer.links] : answer.links;
    for (const link of found) {
      if (link !== "" && !this.links.includes(link)) {
        this.links.push(link);
      }
    }
  }

  /** The link to the one record it changed, or `null`. */
  get link(): string | null {
    return this.links.length === 1 ? (this.links[0] ?? null) : null;
  }
}

/** Run `body` with `group` the invocation group every call inside is a step of. */
export function withinInvocation<T>(group: InvocationGroup, body: () => Promise<T>): Promise<T> {
  return storage.run(group, body);
}

/** The invocation group the current call is a step of, when one is open. */
export function currentGroup(): InvocationGroup | null {
  return storage.getStore() ?? null;
}

/** The account's calls, or one workspace's. */
export class Endpoint {
  readonly transport: Transport;
  readonly workspace: string | null;
  /** The surfaces this client has read, by `${workspace}:${app}`. */
  readonly surfaces: Map<string, string>;
  /** Ends the watches this endpoint opened, when its client closes. */
  readonly signal: AbortSignal | null;

  constructor(
    transport: Transport,
    workspace: string | null = null,
    surfaces: Map<string, string> = new Map(),
    signal: AbortSignal | null = null,
  ) {
    this.transport = transport;
    this.workspace = workspace;
    this.surfaces = surfaces;
    this.signal = signal;
  }

  /** The workspace's id, which a workspace's calls name. */
  get id(): string {
    if (this.workspace === null) {
      throw new ProtocolError("this call names one workspace, and this endpoint is the account's");
    }
    return this.workspace;
  }

  /**
   * One tool call, as the invocation group the current call is a step of
   * names it, its refusal raised.
   */
  async call(tool: string, args: Record<string, unknown>): Promise<Record<string, unknown>> {
    const group = currentGroup();
    const named: Group | null =
      group !== null && this.workspace !== null && group.workspace === this.workspace
        ? { id: group.id, title: group.title }
        : null;
    return this.transport.call(this.workspace, tool, args, named);
  }

  /**
   * The resource at `path`, from `cursor`: a record read as its first
   * record's fields, and any app's read carries its surface's version.
   */
  async read(
    path: string,
    cursor: string | null = null,
    surface: string | null = null,
  ): Promise<Record<string, unknown>> {
    if (surface !== null) {
      await this.checkSurface(path, surface);
    }
    const args: Record<string, unknown> = { path };
    if (cursor !== null) {
      args.cursor = cursor;
    }
    return recordOf(recordResource(path, await this.call("tilo_read", args)));
  }

  /**
   * What the app's window shows for `path`, as the person sees it. `picture`
   * asks for a PNG too, named by a `tilo.content:` reference.
   */
  async view(path: string, picture = false, scale: number | null = null): Promise<View> {
    const args: Record<string, unknown> = { path, picture };
    if (scale !== null) {
      args.scale = scale;
    }
    const answer = await this.call("tilo_view", args);
    if (typeof answer.text !== "string") {
      throw new ProtocolError(`the workspace answered no view for ${path}`);
    }
    return answer as unknown as View;
  }

  /**
   * One action, its arguments the body: its result, the `Invocation` that
   * follows it, or `AskPending` when it waits for the person. `surface` is
   * the app's version of it, for a call made through generated types;
   * `invocationId` names the call, so a retry with the same one runs once.
   */
  async action(
    path: string,
    args: Record<string, unknown> = {},
    surface: string | null = null,
    invocationId: string | null = null,
  ): Promise<unknown> {
    if (surface !== null) {
      await this.checkSurface(path, surface);
    }
    const answer = await this.invoke(path, args, invocationId ?? crypto.randomUUID());
    this.note(answer);
    return this.outcome(answer, path);
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
    if (surface !== null) {
      await this.checkSurface(path, surface);
    }
    const answer = await this.invoke(
      `${path}#group`,
      { actions: members },
      crypto.randomUUID(),
    );
    this.note(answer);
    if (answer.status === "pending" || answer.status === "asked") {
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
    const ask = recordOf(answer.ask);
    const url = textOf(ask.url);
    return new AskPending(url, run, ask, async () => {
      return recordOf(resultOf(await this.wait(run)));
    });
  }

  /** Follow an invocation until it is no longer pending, its whole answer. */
  async wait(invocation: string, timeoutMs: number | null = null): Promise<ActionAnswer> {
    const deadline = timeoutMs === null ? null : Date.now() + timeoutMs;
    let pauseMs = 200;
    for (;;) {
      const answer = await this.invocation(invocation);
      if (answer.status !== "pending" && answer.status !== "asked") {
        return answer;
      }
      if (deadline !== null && Date.now() >= deadline) {
        throw new ProtocolError(`the invocation ${invocation} is still running`);
      }
      const wait = deadline === null ? pauseMs : Math.max(0, Math.min(pauseMs, deadline - Date.now()));
      await sleep(wait);
      pauseMs = Math.min(pauseMs * 2, 1_000);
    }
  }

  /** The invocation as it stands now, without waiting. */
  async invocation(invocation: string): Promise<ActionAnswer> {
    return this.status(invocation);
  }

  /** Stop an invocation. */
  async cancel(invocation: string): Promise<ActionAnswer> {
    return this.status(invocation, { cancel: true });
  }

  /** Take back what an invocation changed: the one call, or every call of its group. */
  async undo(invocation: string): Promise<ActionAnswer> {
    return this.status(invocation, { undo: true });
  }

  /**
   * Run a tilolang program as one step, in the program session `session` names (an earlier
   * answer's `session`), or a new one.
   */
  async program(source: string, session: string | null = null): Promise<ProgramAnswer> {
    const args: Record<string, unknown> = { program: source };
    if (session !== null) {
      args.session = session;
    }
    const answer = await this.call("tilo", args);
    if (typeof answer.text !== "string") {
      // A program refused before it ran says why in the words a person reads.
      const message = textOf(recordOf(answer.error).message);
      if (message !== null) {
        answer.text = message;
      }
    }
    return programAnswerOf(answer);
  }

  /**
   * An invocation's events, from started to succeeded, failed, cancelled or
   * refused. The watch ends with it.
   */
  async *output(invocation: string): AsyncGenerator<Event> {
    let cursor: string | null = null;
    while (this.signal === null || !this.signal.aborted) {
      const answer = await this.call("tilo_watch", watchArgs(cursor, null));
      cursor = cursorOf(answer) ?? cursor;
      for (const raw of arrayOf(answer.events)) {
        const event = eventOfTool(raw, cursor);
        if (event.kind !== "invocation" || event.invocation !== invocation) {
          continue;
        }
        yield event;
        if (event.phase !== null && ENDED.has(event.phase)) {
          return;
        }
      }
    }
  }

  /**
   * The workspace's events as they happen, `path`'s alone or everything under
   * it: each change, each invocation starting or finishing, and who is there.
   * The cursor is kept, so a watch that ends is read on from where it stopped.
   */
  async *events(path: string | null = null): AsyncGenerator<Event> {
    let cursor: string | null = null;
    while (this.signal === null || !this.signal.aborted) {
      try {
        const answer = await this.call("tilo_watch", watchArgs(cursor, path));
        cursor = cursorOf(answer) ?? cursor;
        for (const raw of arrayOf(answer.events)) {
          yield eventOfTool(raw, cursor);
        }
      } catch (error) {
        if (!(error instanceof Unreachable) || (this.signal?.aborted ?? false)) {
          return;
        }
        await sleep(RECONNECT_MS);
      }
    }
  }

  /** The invocation status `tilo_status` answers, with `flags` (`cancel`, `undo`). */
  private async status(
    invocation: string,
    flags: Record<string, unknown> = {},
  ): Promise<ActionAnswer> {
    return actionAnswerOfTool(
      await this.call("tilo_status", { invocation_id: invocation, ...flags }),
    );
  }

  /** One action under the invocation id `invocationId`, its answer as `tilo_do` names it. */
  private async invoke(
    path: string,
    args: Record<string, unknown>,
    invocationId: string,
  ): Promise<ActionAnswer> {
    return actionAnswerOfTool(
      await this.call("tilo_do", {
        path,
        args,
        wait_ms: ACTION_WAIT_MS,
        invocation_id: invocationId,
      }),
    );
  }

  /** What a call's answer is: its result, an `Invocation` to follow, or the ask. */
  private outcome(answer: ActionAnswer, path: string): unknown {
    const ask =
      answer.ask ?? (answer.status === "asked" ? recordOf(answer.raw.record) : null);
    if (answer.status === "asked" || ask !== null) {
      throw this.askOf({ ...answer, ask: ask ?? {} });
    }
    if (answer.status === "pending") {
      return new Invocation(this, answer.invocation ?? "", answer.path ?? path);
    }
    return resultOf(answer);
  }

  /** Note a call's answer in the invocation group it is a step of. */
  private note(answer: ActionAnswer): void {
    const group = currentGroup();
    if (group !== null && this.workspace !== null && group.workspace === this.workspace) {
      group.note(answer);
    }
  }

  /**
   * Refuse a call made through types written for another surface of the app
   * than the workspace publishes. The app's surface is read once per
   * workspace and app, and each match is cached.
   */
  private async checkSurface(path: string, sent: string): Promise<void> {
    if (!sent.includes("@")) {
      throw new ProtocolError("a generated type's surface is <app>@<digest>");
    }
    const app = path.split("/")[0] ?? "";
    const key = `${this.id}\u0000${app}`;
    let published = this.surfaces.get(key);
    if (published === undefined) {
      const read = await this.call("tilo_read", { path: app });
      const readApp = textOf(read.app);
      const surface = textOf(read.surface);
      published = readApp !== null && surface !== null ? surfaceVersion(readApp, surface) : "";
      if (published !== "") {
        // Only a match is cached: an app installed later is read again.
        this.surfaces.set(key, published);
      }
    }
    if (published === sent) {
      return;
    }
    throw new SurfaceChanged(
      published === ""
        ? `these types were made for ${sent}, and the workspace's ${app} publishes no surface: it is not installed there, or has not published yet`
        : `these types were made for ${sent}; the workspace's app publishes ${published}`,
      [`write the types again: tilo sdk gen -w ${this.id}`],
    );
  }
}

/** `<app>@<the first 16 hex of its surface's SHA-256>`. */
export function surfaceVersion(app: string, surface: string): string {
  return `${app}@${bytesToHex(sha256(new TextEncoder().encode(surface))).slice(0, 16)}`;
}

/**
 * What one tool call's answer is, its `invocation_id` as the `ActionAnswer`
 * the rest of the SDK reads (`invocation`).
 */
function actionAnswerOfTool(value: Record<string, unknown>): ActionAnswer {
  const fields: Record<string, unknown> = { ...value };
  const invocation = fields.invocation_id;
  delete fields.invocation_id;
  if (invocation !== undefined) {
    fields.invocation = invocation;
  }
  return actionAnswerOf(fields);
}

/**
 * A record read (`record:<app>:<record>`, as the record API answers it,
 * `{records}`) is the resource at its path whose fields are the record's
 * value; any other read is the resource it already is.
 */
function recordResource(path: string, read: Record<string, unknown>): Record<string, unknown> {
  const records = Array.isArray(read.records) ? read.records : null;
  const resource: Record<string, unknown> =
    records === null
      ? read
      : {
          path,
          fields:
            records
              .map((record) => recordOf(record).value)
              .find((value) => value !== undefined) ?? {},
        };
  const app = textOf(resource.app);
  const surface = textOf(resource.surface);
  if (app !== null && surface !== null) {
    resource.surface_version = surfaceVersion(app, surface);
  }
  return resource;
}

/** One event of a `tilo_watch` answer, as the SDK's `Event` reads it. */
function eventOfTool(raw: Record<string, unknown>, cursor: string | null): Event {
  const data: Record<string, unknown> = { ...raw };
  const kind = textOf(data.kind) ?? "change";
  delete data.kind;
  const invocation = data.invocation_id;
  delete data.invocation_id;
  if (invocation !== undefined) {
    data.invocation = invocation;
  }
  return eventOf(kind, data, cursor);
}

/** What a watch asks: how long to wait, and where it reads on from. */
function watchArgs(cursor: string | null, path: string | null): Record<string, unknown> {
  const args: Record<string, unknown> = { wait_ms: WATCH_WAIT_MS };
  if (cursor !== null) {
    args.cursor = cursor;
  }
  if (path !== null) {
    args.path = path;
  }
  return args;
}

/** The cursor a watch's answer carries, when it names a new one. */
function cursorOf(answer: Record<string, unknown>): string | null {
  const cursor = textOf(answer.cursor);
  return cursor === null || cursor === "" ? null : cursor;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
