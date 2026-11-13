/** One workspace: what is in it, what to do there, and who is there. */

import type { Tilo } from "./client.ts";
import { type Endpoint, RunGroup, withinRun } from "./endpoint.ts";
import {
  type Event,
  type Hit,
  hitOf,
  type Invitation,
  invitationOf,
  type Listing,
  Listing as ListingValue,
  type Member,
  memberOf,
  type MintedKey,
  type ProgramAnswer,
  type WorkspaceInfo,
} from "./models.ts";
import { ProtocolError, refusalOf } from "./errors.ts";
import { resourceProxy, type Resource, type Surface } from "./resource.ts";
import { query as queryOf, routes } from "./routes.ts";
import { collect } from "./transport.ts";

const RUN_PREFIX = "sdk:";

/** A file's bytes, as they go up: one upload's own type, when the caller knows it. */
export type FileBytes = Uint8Array | ArrayBuffer | Blob;

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
   * The host names content by its hash, so a byte sequence goes up once,
   * whatever a caller would have called the file.
   */
  async upload(data: FileBytes): Promise<string> {
    const endpoint = await this.workspace.endpoint();
    const contentType =
      data instanceof Blob && data.type !== "" ? data.type : "application/octet-stream";
    const answer = await endpoint.answer("POST", routes.uploads(endpoint.id), {
      data: await bytesOfFile(data),
      headers: { "content-type": contentType },
    });
    if (typeof answer.content !== "string" || answer.content === "") {
      throw new ProtocolError("the host took the upload without naming the content");
    }
    return answer.content;
  }

  /** The bytes of `content`, a `tilo.content:<hash>` in this workspace. */
  async download(content: string): Promise<Uint8Array> {
    const endpoint = await this.workspace.endpoint();
    const route = routes.content(endpoint.id, content);
    const reply = await endpoint.request("GET", route, { stream: true, idempotent: false });
    if (reply.status >= 400) {
      throw refusalOf(reply.status, reply.body);
    }
    return reply.chunks === null ? new Uint8Array() : collect(reply.chunks);
  }
}

async function bytesOfFile(data: FileBytes): Promise<Uint8Array> {
  if (data instanceof Uint8Array) {
    return data;
  }
  if (data instanceof ArrayBuffer) {
    return new Uint8Array(data);
  }
  return new Uint8Array(await data.arrayBuffer());
}

/** The keys that reach this one workspace. */
export class WorkspaceKeys {
  readonly workspace: Workspace;

  constructor(workspace: Workspace) {
    this.workspace = workspace;
  }

  /** A key for an agent to act in this workspace, narrower than your own. */
  async mint(name: string | null = null, expiresIn: number | null = null): Promise<MintedKey> {
    return this.workspace.client.mintKey(await this.workspace.id(), name, expiresIn);
  }
}

/**
 * The people in this workspace, and bringing someone else in.
 *
 * Inviting takes a key you created reaching all your workspaces, into one you
 * administer, and invites as an editor at most.
 */
export class Members {
  readonly workspace: Workspace;

  constructor(workspace: Workspace) {
    this.workspace = workspace;
  }

  /** The link that brings one person in as `role`. */
  async invite(
    options: { role?: "viewer" | "commenter" | "editor"; email?: string } = {},
  ): Promise<Invitation> {
    const endpoint = await this.workspace.endpoint();
    const json: Record<string, unknown> = { role: options.role ?? "viewer" };
    if (options.email !== undefined) {
      json.email = options.email;
    }
    return invitationOf(
      await endpoint.answer("POST", routes.invitations(endpoint.id), { json }),
    );
  }

  /** The members of the workspace, each with their role. */
  async list(): Promise<Member[]> {
    const listed = await this.#listed();
    return listed.members
      .filter((row) => row !== null && typeof row === "object")
      .map((row) => memberOf(row as Record<string, unknown>));
  }

  async #listed(): Promise<{ members: unknown[] }> {
    const endpoint = await this.workspace.endpoint();
    const answer = await endpoint.answer("GET", routes.members(endpoint.id), {
      idempotent: false,
    });
    return { members: Array.isArray(answer.members) ? answer.members : [] };
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
    return `Workspace(${this.#info?.name || this.ref})`;
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

  /** @internal This workspace's calls. */
  async endpoint(): Promise<Endpoint> {
    return this.client.at(await this.id());
  }

  /** The apps, an app's resources, or one resource's children and actions. */
  async ls(
    path: string | null = null,
    options: { kind?: string | null; limit?: number | null; cursor?: string | null } = {},
  ): Promise<Listing> {
    const endpoint = await this.endpoint();
    const page = async (cursor: string | null): Promise<Listing> => {
      const fields = queryOf({
        path,
        kind: options.kind,
        limit: options.limit,
        cursor,
      });
      const answer = await endpoint.answer(
        "GET",
        `${routes.resources(endpoint.id)}${fields}`,
        { idempotent: false },
      );
      return new ListingValue(answer, (next) => page(next));
    };
    return page(options.cursor ?? null);
  }

  /** One resource's answer, as the API gives it. */
  async readRaw(path: string): Promise<Record<string, unknown>> {
    const endpoint = await this.endpoint();
    return endpoint.answer("GET", routes.resource(endpoint.id, path), { idempotent: false });
  }

  /** A resource read whole: its fields, its children and its actions. */
  async read(path: string): Promise<Resource> {
    return (await this.resource(path).load()) as Resource;
  }

  /**
   * A resource as a typed object: fields, actions and field edits. `surface`
   * is the app's version the types were made from, which generated types
   * pass, and which every call through them sends.
   */
  resource(path: string, surface: Surface = null): Resource {
    return resourceProxy(this, path, surface);
  }

  /** Search the workspace by words, best first. */
  async find(words: string, app: string | null = null, limit: number | null = null): Promise<Hit[]> {
    const endpoint = await this.endpoint();
    const answer = await endpoint.answer(
      "GET",
      `${routes.search(endpoint.id)}${queryOf({ q: words, app, limit })}`,
      { idempotent: false },
    );
    return Array.isArray(answer.hits)
      ? answer.hits
          .filter((hit): hit is Record<string, unknown> => hit !== null && typeof hit === "object")
          .map(hitOf)
      : [];
  }

  /**
   * One call in this workspace, `app/resource#action`: its result, the `Run`
   * that follows it, or `AskPending` when it waits for the person. `surface`
   * is sent as `Tilo-Surface` for a call made through generated types.
   */
  async call(
    path: string,
    args: Record<string, unknown> = {},
    surface: string | null = null,
  ): Promise<unknown> {
    return (await this.endpoint()).action(path, args, surface);
  }

  /** Run one action: its result, or the `Run` that follows it. */
  async do(path: string, args: Record<string, unknown> = {}): Promise<unknown> {
    return this.call(path, args);
  }

  /** Run a tilolang program as one step. */
  async program(source: string, confirm: boolean | null = null): Promise<ProgramAnswer> {
    return (await this.endpoint()).program(source, confirm);
  }

  /** Each change, run event and presence change, the cursor kept. */
  async *watch(path: string | null = null): AsyncGenerator<Event> {
    yield* (await this.endpoint()).events(path);
  }

  /**
   * The link to hand a person for what `path` names (`notes/Todo`). It opens
   * the record in its app for the workspace's members and carries no secret:
   * anyone else must be invited first. `mode` is `app` (the app alone, the
   * default) or `desktop`.
   */
  async link(path: string, options: { mode?: "app" | "desktop" | null } = {}): Promise<string> {
    const endpoint = await this.endpoint();
    const answer = await endpoint.answer(
      "GET",
      `${routes.links(endpoint.id)}${queryOf({ path, mode: options.mode })}`,
      { idempotent: false },
    );
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
    const group = new RunGroup(await this.endpoint(), title, runId(title));
    return withinRun(group, () => body(group));
  }
}
