/** One workspace: what is in it, what to do there, and who is there. */

import type { Tilo } from "./client.ts";
import { Link, RunGroup, withinRun } from "./link.ts";
import {
  hitOf,
  type Event,
  type Hit,
  type Listing,
  Listing as ListingValue,
  type MintedKey,
  type ProgramAnswer,
  type WorkspaceInfo,
} from "./models.ts";
import { ProtocolError, Refused } from "./errors.ts";
import { resourceProxy, type Resource } from "./resource.ts";
import type { Answer } from "./wire.ts";

const RUN_PREFIX = "sdk:";

/** File bytes into the workspace: upload first, then an action takes the reference. */
export class Files {
  readonly workspace: Workspace;

  constructor(workspace: Workspace) {
    this.workspace = workspace;
  }

  /**
   * Upload bytes and answer their `tilo.content:<hash>` reference.
   *
   * The door names content by its hash, so `name` is a caller's own label,
   * kept beside the reference rather than sent.
   */
  async upload(data: Uint8Array | ArrayBuffer | Blob, name: string | null = null): Promise<string> {
    const workspace = await this.workspace.id();
    const transport = this.workspace.client.transport;
    const session = transport.session(workspace);
    if (session.id === null) {
      await session.open();
    }
    const bytes =
      data instanceof Blob
        ? new Uint8Array(await data.arrayBuffer())
        : data instanceof Uint8Array
          ? data
          : new Uint8Array(data);
    const response = await transport.request("PUT", `/w/${workspace}/content`, {
      data: bytes,
      session: session.id,
    });
    let body: Record<string, unknown> = {};
    try {
      const parsed = await response.clone().json();
      if (parsed && typeof parsed === "object") {
        body = parsed as Record<string, unknown>;
      }
    } catch {
      body = {};
    }
    if (response.status >= 400 || typeof body.content !== "string") {
      const error = body.error;
      if (error && typeof error === "object") {
        const detail = error as Record<string, unknown>;
        throw new Refused(
          String(detail.code ?? "upload_failed"),
          String(detail.message ?? ""),
          Array.isArray(detail.next_steps) ? detail.next_steps.map(String) : [],
        );
      }
      throw new ProtocolError(
        `the door answered the upload with ${response.status}`,
        response.status,
      );
    }
    void name;
    return body.content;
  }
}

/** The keys that reach this one workspace. */
export class WorkspaceKeys {
  readonly workspace: Workspace;

  constructor(workspace: Workspace) {
    this.workspace = workspace;
  }

  /** A key for an agent to act in this workspace, narrower than your own. */
  async mint(name: string | null = null, expiresIn: number | null = null): Promise<MintedKey> {
    const workspace = await this.workspace.id();
    return this.workspace.client.mintKey(workspace, name, expiresIn);
  }
}

function runId(title: string): string {
  const slug = title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40);
  return `${RUN_PREFIX}${slug || "run"}:${crypto.randomUUID().slice(0, 8)}`;
}

/**
 * One workspace, named by its id or by its name.
 */
export class Workspace {
  readonly client: Tilo;
  readonly ref: string;
  readonly files: Files;
  readonly keys: WorkspaceKeys;
  #info: WorkspaceInfo | null = null;

  constructor(client: Tilo, ref: string) {
    this.client = client;
    this.ref = ref;
    this.files = new Files(this);
    this.keys = new WorkspaceKeys(this);
  }

  toString(): string {
    return `Workspace(${this.#info?.name ?? this.ref})`;
  }

  async info(): Promise<WorkspaceInfo> {
    if (this.#info === null) {
      this.#info = await this.client.workspaceInfo(this.ref);
    }
    return this.#info;
  }

  async id(): Promise<string> {
    return (await this.info()).id;
  }

  get link(): Link {
    if (this.#info === null) {
      throw new ProtocolError(
        `the workspace ${this.ref} is not resolved yet: use an awaited call first`,
      );
    }
    return new Link(this.client.transport, this.#info.id);
  }

  /** One typed call in this workspace, its session opened on first use. */
  async call(tool: string, args: Record<string, unknown>): Promise<Answer> {
    await this.info();
    return this.link.call(tool, args);
  }

  /** The apps, an app's resources, or one resource's children and actions. */
  async ls(
    path: string | null = null,
    options: { kind?: string | null; limit?: number | null; cursor?: string | null } = {},
  ): Promise<Listing> {
    const args: Record<string, unknown> = {};
    if (path !== null) {
      args.path = path;
    }
    if (options.kind != null) {
      args.kind = options.kind;
    }
    if (options.limit != null) {
      args.limit = options.limit;
    }
    if (options.cursor != null) {
      args.cursor = options.cursor;
    }
    const answer = await this.call("tilo_ls", args);
    return new ListingValue(answer, (cursor) =>
      this.ls(path, { ...options, cursor }),
    );
  }

  /** A resource read whole: its fields, its children and its actions. */
  async read(path: string): Promise<Resource> {
    return (await this.resource(path).load()) as Resource;
  }

  /** A resource as a typed object: fields, actions and field edits. */
  resource(path: string): Resource {
    return resourceProxy(this, path);
  }

  /** Search the workspace by words, best first. */
  async find(query: string, app: string | null = null, limit: number | null = null): Promise<Hit[]> {
    const args: Record<string, unknown> = { query };
    if (app !== null) {
      args.app = app;
    }
    if (limit !== null) {
      args.limit = limit;
    }
    const answer = await this.call("tilo_find", args);
    return Array.isArray(answer.hits)
      ? answer.hits.map((hit) => hitOf(hit as Record<string, unknown>))
      : [];
  }

  /** Run one action: its result, or the `Run` that follows it. */
  async do(path: string, args: Record<string, unknown> = {}): Promise<unknown> {
    await this.info();
    return this.link.outcome(path, args);
  }

  /** How a pending invocation went, or stop it. */
  async status(invocationId: string, cancel = false): Promise<Answer> {
    await this.info();
    return this.link.status(invocationId, cancel);
  }

  /** Run a tilolang program as this run's task. */
  async program(source: string, confirm: boolean | null = null): Promise<ProgramAnswer> {
    await this.info();
    return this.link.program(source, confirm);
  }

  /** Each change, run event and presence change, the cursor kept. */
  async *watch(path: string | null = null, waitMs: number | null = null): AsyncGenerator<Event> {
    await this.info();
    for await (const event of this.link.watch(path, waitMs)) {
      yield event;
    }
  }

  /**
   * Run `body` as one run: every call inside carries its id and title, so the
   * apps record one undo step and history shows the title.
   */
  async run<T>(title: string, body: (run: RunGroup) => Promise<T>): Promise<T> {
    await this.info();
    const group = new RunGroup(this.link, title, runId(title));
    return withinRun(group, () => body(group));
  }

  /** A key for an agent to act in this workspace; `keys.mint` is the same. */
  mintMcp(name: string | null = null, expiresIn: number | null = null): Promise<MintedKey> {
    return this.keys.mint(name, expiresIn);
  }
}
