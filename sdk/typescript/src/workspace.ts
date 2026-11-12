/** One workspace: what is in it, what to do there, and who is there. */

import type { Tilo } from "./client.ts";
import { Endpoint, RunGroup, withinRun } from "./endpoint.ts";
import {
  hitOf,
  invitationOf,
  memberOf,
  type Event,
  type Hit,
  type Invitation,
  type Listing,
  Listing as ListingValue,
  type Member,
  type MintedKey,
  type ProgramAnswer,
  type WorkspaceInfo,
} from "./models.ts";
import { ProtocolError, Refused, keyRefusal } from "./errors.ts";
import { resourceProxy, type Resource } from "./resource.ts";
import type { Answer } from "./wire.ts";

const RUN_PREFIX = "sdk:";

/**
 * File bytes in and out of the workspace, named `tilo.content:<hash>`.
 *
 * Upload first, then an action takes the reference; an action that makes a
 * file (Writer's `export-content`) answers one to download.
 */
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
      const typed = response.status === 401 ? keyRefusal(error, response.status) : null;
      if (typed !== null) {
        throw typed;
      }
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

  /** The bytes of `content`, a `tilo.content:<hash>` in this workspace. */
  async download(content: string): Promise<Uint8Array> {
    const pieces: Uint8Array[] = [];
    let received = 0;
    for (;;) {
      const answer = await this.workspace.call("tilo_get", { content, offset: received });
      const { bytes, size } = answer;
      if (typeof bytes !== "string" || typeof size !== "number") {
        throw new ProtocolError(`tilo_get answered no bytes for ${content}`);
      }
      const piece = Uint8Array.from(atob(bytes), (char) => char.charCodeAt(0));
      pieces.push(piece);
      received += piece.length;
      if (received >= size) {
        break;
      }
      if (piece.length === 0) {
        throw new ProtocolError(`tilo_get stopped at ${received} of ${size} bytes of ${content}`);
      }
    }
    const whole = new Uint8Array(received);
    let at = 0;
    for (const piece of pieces) {
      whole.set(piece, at);
      at += piece.length;
    }
    return whole;
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

/**
 * The people in this workspace, and bringing someone else in.
 *
 * Inviting takes a key you created reaching all your workspaces, into one you
 * administer, and invites as an editor at most.
 */
export class Members {
  static readonly PATH = "workspace/members";
  readonly workspace: Workspace;

  constructor(workspace: Workspace) {
    this.workspace = workspace;
  }

  /** The link that brings one person in as `role`. */
  async invite(
    options: { role?: "viewer" | "commenter" | "editor"; email?: string } = {},
  ): Promise<Invitation> {
    const args: Record<string, unknown> = { role: options.role ?? "viewer" };
    if (options.email !== undefined) args.email = options.email;
    const result = await this.workspace.do(`${Members.PATH}#invite`, args);
    if (result === null || typeof result !== "object" || Array.isArray(result)) {
      throw new ProtocolError("the door invited without answering a link");
    }
    return invitationOf(result as Record<string, unknown>);
  }

  /** The members of the workspace, each with their role. */
  async list(): Promise<Member[]> {
    const result = (await this.workspace.do(`${Members.PATH}#list`)) as
      | { members?: unknown }
      | null;
    const rows = Array.isArray(result?.members) ? result.members : [];
    return rows
      .filter((row): row is Record<string, unknown> => row !== null && typeof row === "object")
      .map(memberOf);
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
  readonly members: Members;
  #info: WorkspaceInfo | null = null;

  constructor(client: Tilo, ref: string) {
    this.client = client;
    this.ref = ref;
    this.files = new Files(this);
    this.keys = new WorkspaceKeys(this);
    this.members = new Members(this);
  }

  toString(): string {
    return `Workspace(${this.#info?.name ?? this.ref})`;
  }

  /** The workspace as the account lists it: its id, name, host and your role. */
  async info(): Promise<WorkspaceInfo> {
    if (this.#info === null) {
      this.#info = await this.client.workspaceInfo(this.ref);
    }
    return this.#info;
  }

  /** Its id. */
  async id(): Promise<string> {
    return (await this.info()).id;
  }

  /** @internal */
  get endpoint(): Endpoint {
    if (this.#info === null) {
      throw new ProtocolError(
        `the workspace ${this.ref} is not resolved yet: use an awaited call first`,
      );
    }
    return new Endpoint(this.client.transport, this.#info.id);
  }

  /** One typed call in this workspace, its session opened on first use. */
  async call(tool: string, args: Record<string, unknown>): Promise<Answer> {
    await this.info();
    return this.endpoint.call(tool, args);
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

  /**
   * A record of `app` as plain data, `{revision, records: [{record, value}]}`: the app's
   * record of the workspace for an empty `record`. `parts` reads the chain of parts the
   * record is the base of (a chat's messages); `after` waits for a write past that
   * revision. Chat's conversations are read so.
   */
  async record(
    app: string,
    record = "",
    options: { parts?: boolean; after?: string | null } = {},
  ): Promise<Record<string, unknown>> {
    const args: Record<string, unknown> = { path: `record:${app}:${record}`, parts: options.parts ?? false };
    if (options.after != null) {
      args.after = options.after;
    }
    return this.call("tilo_read", args);
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
    return this.endpoint.outcome(path, args);
  }

  /** How a pending invocation went, or stop it. */
  async status(invocationId: string, cancel = false): Promise<Answer> {
    await this.info();
    return this.endpoint.status(invocationId, cancel);
  }

  /** Run a tilolang program as this run's task. */
  async program(source: string, confirm: boolean | null = null): Promise<ProgramAnswer> {
    await this.info();
    return this.endpoint.program(source, confirm);
  }

  /** Each change, run event and presence change, the cursor kept. */
  async *watch(path: string | null = null, waitMs: number | null = null): AsyncGenerator<Event> {
    await this.info();
    for await (const event of this.endpoint.watch(path, waitMs)) {
      yield event;
    }
  }

  /**
   * The link to hand a person for what `path` names (`notes/Todo`). It opens
   * the record in its app for the workspace's members and carries no secret:
   * anyone else must be invited first. `mode` is `app` (the app alone, the
   * default) or `desktop`.
   */
  async link(path: string, options: { mode?: "app" | "desktop" | null } = {}): Promise<string> {
    const args: Record<string, unknown> = { path };
    if (options.mode != null) {
      args.mode = options.mode;
    }
    const answer = await this.call("tilo_link", args);
    if (typeof answer.link !== "string" || answer.link === "") {
      throw new ProtocolError(`the workspace answered no link for ${path}`);
    }
    return answer.link;
  }

  /**
   * Run `body` as one run: every call inside carries its id and title, so the
   * apps record one undo step and history shows the title.
   */
  async run<T>(title: string, body: (run: RunGroup) => Promise<T>): Promise<T> {
    await this.info();
    const group = new RunGroup(this.endpoint, title, runId(title));
    return withinRun(group, () => body(group));
  }

  /** A key for an agent to act in this workspace; `keys.mint` is the same. */
  mintMcp(name: string | null = null, expiresIn: number | null = null): Promise<MintedKey> {
    return this.keys.mint(name, expiresIn);
  }
}
