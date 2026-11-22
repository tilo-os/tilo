/** One workspace: what is in it, what to do there, and who is there. */

import type { Tilo } from "./client.ts";
import { type Endpoint, InvocationGroup, withinInvocation } from "./endpoint.ts";
import {
  actionAnswerOf,
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
  InvocationList,
  type View,
  type WorkspaceExport,
  type WorkspaceInfo,
  resultOf,
  workspaceOf,
} from "./models.ts";
import { ProtocolError, refusalOf } from "./errors.ts";
import { resourceProxy, type Resource, type Surface } from "./resource.ts";
import { query as queryOf, routes } from "./routes.ts";
import { collect } from "./transport.ts";

const INVOCATION_PREFIX = "sdk:";

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
 * administer, and invites as an editor at most, or as an administrator where
 * you may make administrators.
 */
export class Members {
  readonly workspace: Workspace;

  constructor(workspace: Workspace) {
    this.workspace = workspace;
  }

  /**
   * The link that brings one person in as `role`. `administrator` gives them
   * an editor's part and the administrator grants, where you may make
   * administrators.
   */
  async invite(
    options: {
      role?: "viewer" | "commenter" | "editor" | "administrator";
      email?: string;
    } = {},
  ): Promise<Invitation> {
    const endpoint = await this.workspace.endpoint();
    const args: Record<string, unknown> = { role: options.role ?? "viewer" };
    if (options.email !== undefined) {
      args.invitee_name = options.email;
    }
    const invited = await endpoint.action("workspace/invitations#invite", args);
    const invitation = invitationOf((invited ?? {}) as Record<string, unknown>);
    // Tilo sends no mail to an address it names: the link is handed over.
    if (options.email === undefined) {
      return invitation;
    }
    const next =
      invitation.next ?? `Tilo sends no email: give the person this link to send to ${options.email}`;
    return { ...invitation, email: options.email, next };
  }

  /** The members of the workspace, each with their role. */
  async list(): Promise<Member[]> {
    const endpoint = await this.workspace.endpoint();
    const listed = await endpoint.action("workspace/members#list");
    return (Array.isArray(listed) ? listed : [])
      .filter((row) => row !== null && typeof row === "object")
      .map((row) => memberOf(row as Record<string, unknown>));
  }
}

function invocationId(title: string): string {
  const slug = title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40);
  return `${INVOCATION_PREFIX}${slug || "invocation"}:${crypto.randomUUID().slice(0, 8)}`;
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
   * One call in this workspace, `app/resource#action`: its result, the `Invocation`
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

  /** Run one action: its result, or the `Invocation` that follows it. */
  async do(path: string, args: Record<string, unknown> = {}): Promise<unknown> {
    return this.call(path, args);
  }

  /**
   * The workspace's invocations in records this key may read, newest first,
   * whoever made them: an `InvocationList` of `InvocationInfo`s, `limit` to a
   * page (50 unless said, at most 200); `for await` reads on to the oldest. Of
   * each it says only what lies in those records, and its totals count only
   * those. Each says who made it, how it stands (`waiting` for the person,
   * `running`, `ended`, `failed`, `cancelled`, `undone`), when, what it changed
   * and whether it can still be taken back. Its `id` is what an `Invocation`
   * reads, cancels and undoes; one the host no longer remembers, after it
   * restarted, is listed from the workspace's records without one.
   */
  async invocations(
    options: { limit?: number | null; cursor?: string | null } = {},
  ): Promise<InvocationList> {
    const endpoint = await this.endpoint();
    const page = async (cursor: string | null): Promise<InvocationList> => {
      // The workspace's invocations are a resource of its own: `invocations#list` answers a page.
      const args: Record<string, unknown> = {};
      if (options.limit !== undefined && options.limit !== null) {
        args.limit = options.limit;
      }
      if (cursor !== null) {
        args.cursor = cursor;
      }
      const answer = await endpoint.action("invocations#list", args);
      if (typeof answer !== "object" || answer === null) {
        throw new ProtocolError("the workspace answered no page of its invocations");
      }
      return new InvocationList(answer as Record<string, unknown>, (next) => page(next));
    };
    return page(options.cursor ?? null);
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
    return (await this.endpoint()).program(source, confirm, session);
  }

  /** Each change, invocation event and presence change, the cursor kept. */
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
   * What the app's window shows for `path`, as the person sees it: every
   * label, value and control in reading order, each control's action and
   * where it stands. With `picture`, the same view as a PNG, named by a
   * `tilo.content:` reference that `files.download` fetches.
   *
   * The app is started without a window when it does not run.
   */
  async view(
    path: string,
    options: { picture?: boolean; scale?: number | null } = {},
  ): Promise<View> {
    const endpoint = await this.endpoint();
    const answer = await endpoint.answer(
      "GET",
      `${routes.views(endpoint.id)}${queryOf({
        path,
        picture: options.picture === true ? true : null,
        scale: options.scale ?? null,
      })}`,
      { idempotent: false },
    );
    if (typeof answer.text !== "string") {
      throw new ProtocolError(`the workspace answered no view for ${path}`);
    }
    return answer as unknown as View;
  }

  /**
   * Everything the workspace holds, read whole: its listing (with its
   * labels) and every resource of every app, as a walk from its apps finds
   * them; the ones that did not read are in `unread`, with why. The files
   * the resources name (`tilo.content:` references) download with
   * `files.download`. Take one before deleting a customer's workspace.
   */
  async export(): Promise<WorkspaceExport> {
    const endpoint = await this.endpoint();
    const answer = await endpoint.answer("GET", routes.export(endpoint.id), { idempotent: false });
    const workspace =
      answer.workspace !== null && typeof answer.workspace === "object"
        ? workspaceOf(answer.workspace as Record<string, unknown>)
        : await this.info();
    return {
      workspace,
      exportedAtMs: Number(answer.exported_at_ms ?? 0),
      resources: Array.isArray(answer.resources)
        ? (answer.resources as Record<string, unknown>[])
        : [],
      unread: Array.isArray(answer.unread)
        ? (answer.unread as { path: string; error: string }[])
        : [],
    };
  }

  /**
   * Let the keys made for this workspace alone run commands in its Linux
   * machine (`terminal#run`) without asking the person each time, here and in
   * the branches made from it; `on = false` asks again from the next command.
   * Turning it on asks the person on their approving device, whichever key
   * asks: it throws `AskPending`, whose `wait()` answers
   * `{ workspace, unattended: true }` once they allow it, or throws `Refused`
   * if they do not. Turning it off asks nobody. A key reaching all their
   * workspaces is still asked, and so is everything else that asks.
   */
  async unattended(on = true): Promise<Record<string, unknown>> {
    const endpoint = await this.endpoint();
    const answer = actionAnswerOf(
      await endpoint.answer("PUT", routes.unattended(endpoint.id), { json: { on } }),
    );
    if (answer.status === "pending") {
      throw endpoint.askOf(answer);
    }
    const result = resultOf(answer);
    return result !== null && typeof result === "object" ? (result as Record<string, unknown>) : {};
  }

  /**
   * Branch this workspace, one of your own, into a new one named `name`, as
   * `tilo.workspaces.branch` does: it starts with everything this one holds
   * and goes on apart from it; you are its only member.
   */
  branch(
    name: string,
    options: { labels?: Record<string, string>; idempotencyKey?: string } = {},
  ): Promise<WorkspaceInfo> {
    return this.client.branchWorkspace(
      this,
      name,
      options.labels ?? {},
      options.idempotencyKey ?? null,
    );
  }

  /**
   * Run `body` as one invocation group: every call inside carries its id and
   * title, so the apps record one undo step and history shows the title.
   */
  async invocation<T>(
    title: string,
    body: (invocation: InvocationGroup) => Promise<T>,
  ): Promise<T> {
    const group = new InvocationGroup(await this.endpoint(), title, invocationId(title));
    return withinInvocation(group, () => body(group));
  }
}
