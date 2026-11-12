/**
 * One endpoint the client calls: the account, or one workspace.
 *
 * Every call here is a `tools/call` of that endpoint's session, with the run
 * the call is a step of when one is open.
 */

import { AsyncLocalStorage } from "node:async_hooks";

import type { Answer } from "./wire.ts";
import { answerOf, askOf, resultOf, type Transport } from "./wire.ts";
import { eventOf, type Event, programAnswerOf, type ProgramAnswer } from "./models.ts";
import { ProtocolError } from "./errors.ts";
import { Run } from "./run.ts";

const storage = new AsyncLocalStorage<RunGroup>();

/**
 * A run a group of calls is attributed to: one undo step per app.
 *
 * `links` are the links to every record its calls changed, each once, in the
 * order first changed; `link` is the one link when they changed one record,
 * else `null`: a run across several records hands over `links`. A link opens
 * the record in its app for the workspace's members and carries no secret.
 */
export class RunGroup {
  readonly endpoint: Endpoint;
  readonly title: string;
  readonly id: string;
  readonly invocations: string[] = [];
  readonly links: string[] = [];

  constructor(endpoint: Endpoint, title: string, id: string) {
    this.endpoint = endpoint;
    this.title = title;
    this.id = id;
  }

  /** @internal */
  note(answer: Answer): void {
    const invocationId = answer.invocation_id;
    if (typeof invocationId === "string" && invocationId !== "") {
      this.invocations.push(invocationId);
    }
    for (const link of linksOf(answer)) {
      if (!this.links.includes(link)) {
        this.links.push(link);
      }
    }
  }

  /** The link to the one record the run changed, or `null`. */
  get link(): string | null {
    return this.links.length === 1 ? (this.links[0] ?? null) : null;
  }

  /** Take the run's changes back, each app one step at a time. */
  undo(): Promise<ProgramAnswer> {
    return this.endpoint.program("undo task");
  }
}

/** The record links an answer carries: a call's `link`, a batch's `links`. */
export function linksOf(answer: Answer): string[] {
  const found: unknown[] = [answer.link];
  if (Array.isArray(answer.links)) {
    found.push(...answer.links);
  }
  return found.filter((link): link is string => typeof link === "string" && link !== "");
}

/** Run `body` with `group` the run every call inside is a step of. */
export function withinRun<T>(group: RunGroup, body: () => Promise<T>): Promise<T> {
  return storage.run(group, body);
}

/** The account's session or one workspace's, and what this call joins. */
export class Endpoint {
  readonly transport: Transport;
  readonly workspace: string | null;

  constructor(transport: Transport, workspace: string | null = null) {
    this.transport = transport;
    this.workspace = workspace;
  }

  /** @internal */
  get session() {
    return this.transport.session(this.workspace);
  }

  /** @internal */
  runMeta(): Record<string, unknown> | null {
    const group = storage.getStore();
    return group === undefined ? null : { id: group.id, title: group.title };
  }

  /** One typed call, refused or asked as the wire says. */
  async call(
    tool: string,
    args: Record<string, unknown> = {},
    grouped = true,
  ): Promise<Answer> {
    const answer = answerOf(
      await this.session.call(tool, args, grouped ? this.runMeta() : null),
    );
    if (grouped) {
      storage.getStore()?.note(answer);
    }
    const ask = askOf(answer, (invocationId) => this.wait(invocationId));
    if (ask !== null) {
      throw ask;
    }
    return answer;
  }

  /** Run one action: its result, or the `Run` that follows it. */
  async outcome(path: string, args: Record<string, unknown>): Promise<unknown> {
    return this.follow(await this.call("tilo_do", { path, args }), path);
  }

  /** @internal */
  follow(answer: Answer, path: string | null = null): unknown {
    if (answer.status === "pending") {
      return new Run(this, String(answer.invocation_id ?? ""), path);
    }
    if (answer.status === "ok") {
      return resultOf(answer);
    }
    return answer;
  }

  /** `tilo_status`: how an invocation went, or stop it. */
  async status(invocationId: string, cancel = false): Promise<Answer> {
    const args: Record<string, unknown> = { invocation_id: invocationId };
    if (cancel) {
      args.cancel = true;
    }
    return answerOf(await this.session.call("tilo_status", args, null));
  }

  /** Follow an invocation until it is done. */
  async wait(invocationId: string, timeoutMs: number | null = null): Promise<Answer> {
    let delay = 200;
    const deadline = timeoutMs === null ? null : Date.now() + timeoutMs;
    for (;;) {
      const answer = await this.status(invocationId);
      if (answer.status !== "pending") {
        return answer;
      }
      if (deadline !== null && Date.now() >= deadline) {
        throw new ProtocolError(`invocation ${invocationId} is still running`);
      }
      await new Promise((resolve) => setTimeout(resolve, delay));
      delay = Math.min(delay * 2, 1000);
    }
  }

  /** The `tilo` tool: a tilolang program run as this run's task. */
  async program(source: string, confirm: boolean | null = null): Promise<ProgramAnswer> {
    const args: Record<string, unknown> = { program: source };
    if (confirm !== null) {
      args.confirm = confirm;
    }
    const answer = await this.call("tilo", args);
    if (answer.status === "pending") {
      throw new ProtocolError("this program did not answer: follow it with tilo_status");
    }
    return programAnswerOf(answer);
  }

  /** Each change, run event and presence change, its cursor kept. */
  async *watch(path: string | null = null, waitMs: number | null = null): AsyncGenerator<Event> {
    let cursor: string | null = null;
    for (;;) {
      const args: Record<string, unknown> = {};
      if (cursor !== null) {
        args.cursor = cursor;
      }
      if (path !== null) {
        args.path = path;
      }
      if (waitMs !== null) {
        args.wait_ms = waitMs;
      }
      const answer = await this.call("tilo_watch", args, false);
      const next = answer.cursor;
      if (typeof next === "string") {
        cursor = next;
      }
      for (const raw of (answer.events as unknown[]) ?? []) {
        if (raw && typeof raw === "object") {
          yield eventOf(raw as Record<string, unknown>);
        }
      }
    }
  }
}
