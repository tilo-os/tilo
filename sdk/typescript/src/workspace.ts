/** One workspace: what is in it, what to do there, and who is there. */

import type { Tilo } from "./client.ts";
import { type Endpoint, InvocationGroup, withinInvocation } from "./endpoint.ts";
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
  InvocationList,
  type View,
  type WorkspaceInfo,
  type WorkspaceExport,
  recordOf,
} from "./models.ts";
import { Invocation } from "./invocation.ts";
import { ProtocolError } from "./errors.ts";
import { resourceProxy, type Resource, type Surface } from "./resource.ts";

const INVOCATION_PREFIX = "sdk:";
/** How many bytes one append of an upload carries. */
const PUT_PIECE = 384 * 1024;
/** How many appends of an upload may be in flight at once. */
const PUTS_AT_ONCE = 8;

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
    const bytes = await bytesOfFile(data);
    const begun = await endpoint.call("tilo_put_begin", { size: bytes.length });
    const upload = begun.upload;
    if (upload === undefined || upload === null) {
      throw new ProtocolError("the host began no upload for the file");
    }
    let inFlight: Promise<unknown>[] = [];
    for (let offset = 0; offset < bytes.length; offset += PUT_PIECE) {
      const piece = bytes.subarray(offset, Math.min(offset + PUT_PIECE, bytes.length));
      inFlight.push(
        endpoint.call("tilo_put_append", {
          upload,
          offset,
          chunk: base64Of(piece),
        }),
      );
      if (inFlight.length >= PUTS_AT_ONCE) {
        await Promise.all(inFlight);
        inFlight = [];
      }
    }
    await Promise.all(inFlight);
    const finished = await endpoint.call("tilo_put_finish", { upload });
    const content = typeof finished.content === "string" ? finished.content : "";
    if (content === "") {
      throw new ProtocolError("the host finished the upload with no content");
    }
    return content;
  }

  /** The bytes of `content`, a `tilo.content:<hash>` in this workspace. */
  async download(content: string): Promise<Uint8Array> {
    const endpoint = await this.workspace.endpoint();
    const pieces: Uint8Array[] = [];
    let offset = 0;
    for (;;) {
      const piece = await endpoint.call("tilo_get", { content, offset });
      const bytes =
        typeof piece.bytes === "string" ? bytesOfBase64(piece.bytes) : new Uint8Array();
      if (bytes.length === 0) {
        break;
      }
      pieces.push(bytes);
      offset += bytes.length;
      const size = typeof piece.size === "number" ? piece.size : null;
      if (size !== null && offset >= size) {
        break;
      }
    }
    return concatOf(pieces);
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

/** Bytes as base64, in pieces small enough for a string's argument list. */
function base64Of(bytes: Uint8Array): string {
  let binary = "";
  const step = 0x8000;
  for (let at = 0; at < bytes.length; at += step) {
    binary += String.fromCharCode(...bytes.subarray(at, at + step));
  }
  return btoa(binary);
}

function bytesOfBase64(text: string): Uint8Array {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

function concatOf(pieces: Uint8Array[]): Uint8Array {
  const whole = new Uint8Array(pieces.reduce((total, piece) => total + piece.length, 0));
  let at = 0;
  for (const piece of pieces) {
    whole.set(piece, at);
    at += piece.length;
  }
  return whole;
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
    const invitation = invitationOf(recordOf(invited));
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

/** Content the workspace keeps, named by the address of its bytes. */
export interface WebCallContent {
  /** The app whose records keep it; the asking app when left out. */
  app?: string;
  /** The address of the bytes: lowercase BLAKE3 hex. */
  hash: string;
  /** The filename the part carries. */
  filename?: string;
  /** The bytes' media type, such as `image/png`. */
  contentType?: string;
}

/** One part of a web call's body: text, JSON, or content the workspace keeps. */
export interface WebCallPart {
  /** The form field's name. */
  name: string;
  /** The part's text. */
  text?: string;
  /** The part's JSON. */
  json?: unknown;
  /** Content the workspace keeps, which the vault reads and sends as the part. */
  content?: WebCallContent;
}

/** A web request made with a key the vault keeps, by reference. */
export interface WebCallOptions {
  /** The address to call, at the service the key belongs to. */
  url: string;
  /** The vault item, a `vault.item:` reference. */
  key: string;
  /** `GET` when left out. */
  method?: string;
  /** Headers to send besides the key. */
  headers?: Record<string, string>;
  /** What to send, as text. Not beside `bodyParts` or `bodyContent`. */
  body?: string;
  /** What to send, as the parts of a multipart body. Not beside `body` or `bodyContent`. */
  bodyParts?: WebCallPart[];
  /** Content the workspace keeps, sent as the whole body. Not beside `body` or `bodyParts`. */
  bodyContent?: WebCallContent;
  /** Why you need it, in one sentence your person reads. */
  reason?: string;
}

/** What a web call answered: the service's status, headers and body, the key struck out. */
export interface WebCallAnswer {
  status: number;
  headers: Record<string, string>;
  body: string;
}

/** Ask the person for a secret, which their vault then keeps for the places named. */
export interface VaultAsk {
  /** What it is: `login`, `key`, `code-generator`, `ssh-key` or `value`. */
  kind: "login" | "key" | "code-generator" | "ssh-key" | "value";
  /** Where it belongs, each a web address or a server; one to four. */
  belongsTo: string[];
  /** The name it signs in with, when it has one. */
  username?: string;
  /** For a key: how its service takes it (`basic`, `header:<name>`, `query:<name>`). */
  sentIn?: string;
}

/** What an app names when it looks for a secret it was moved to: its own tags. */
export interface VaultFind {
  /** The app's id, as the old keeper kept its entry under. */
  app: string;
  /** The purpose the keeper kept it for, such as `bot-token`. */
  purpose: string;
}

/**
 * The person's vault: a secret stays there, and a call uses it by reference.
 *
 * There is no way to read a secret back. `webCall` has the server add the
 * key on the way out and answers the service's reply with the key struck
 * from it; `ask` is the one way a program comes by an item it was not given.
 * `find` names the item a secret an app was moved to was kept as, and
 * `release` hands that item's secret to the app's own process.
 */
export class Vault {
  readonly workspace: Workspace;

  constructor(workspace: Workspace) {
    this.workspace = workspace;
  }

  /** Make a web request with a key the vault keeps, by reference. */
  async webCall(call: WebCallOptions): Promise<WebCallAnswer> {
    const args: Record<string, unknown> = { url: call.url, key: call.key };
    if (call.method !== undefined) {
      args.method = call.method;
    }
    if (call.headers !== undefined) {
      args.headers = call.headers;
    }
    if (call.body !== undefined) {
      args.body = call.body;
    }
    if (call.bodyParts !== undefined) {
      args.bodyParts = call.bodyParts;
    }
    if (call.bodyContent !== undefined) {
      args.bodyContent = call.bodyContent;
    }
    if (call.reason !== undefined) {
      args.reason = call.reason;
    }
    const endpoint = await this.workspace.endpoint();
    const answer = recordOf(await endpoint.webCall(args));
    const headers: Record<string, string> = {};
    for (const [name, value] of Object.entries(recordOf(answer.headers))) {
      headers[name] = String(value);
    }
    return {
      status: typeof answer.status === "number" ? answer.status : 0,
      headers,
      body: typeof answer.body === "string" ? answer.body : "",
    };
  }

  /**
   * The secret of the vault item `item` (`vault.item:…`), handed to this app.
   * Only the process the daemon launched for an app's connection is handed
   * one, and only for an item the workspace granted the app: an item it has no
   * grant on throws `Refused`, and its person is asked once whether it may.
   * The secret is answered to the caller and kept nowhere.
   */
  async release(item: string): Promise<string> {
    return (await this.workspace.endpoint()).release(item);
  }

  /**
   * Ask the person for a password, a key or another secret. They type it into
   * the system's protected field on their own device; the answer is the
   * reference the vault keeps it under, or `null` when they declined. It may
   * throw `AskPending`, which waits for their decision.
   */
  async ask(ask: VaultAsk): Promise<string | null> {
    const args: Record<string, unknown> = { kind: ask.kind, belongsTo: ask.belongsTo };
    if (ask.username !== undefined) {
      args.username = ask.username;
    }
    if (ask.sentIn !== undefined) {
      args.sentIn = ask.sentIn;
    }
    const endpoint = await this.workspace.endpoint();
    const answer = recordOf(await endpoint.action("vault/vault#ask", args));
    return typeof answer.reference === "string" ? answer.reference : null;
  }

  /**
   * The reference of the vault item kept for the app and purpose `tags` names: the item a
   * secret the app was moved to is kept as, found by the tags it carries. `null` when the vault
   * keeps none. It releases nothing, so the person is not asked; releasing the item afterwards
   * is, and names the asking app.
   */
  async find(tags: VaultFind): Promise<string | null> {
    const endpoint = await this.workspace.endpoint();
    return endpoint.find(tags.app, tags.purpose);
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
 * One workspace, named by its id or by its name; named by neither, the one
 * the daemon's connection acts in.
 */
export class Workspace {
  readonly client: Tilo;
  readonly ref: string | null;
  readonly files: Files;
  readonly keys: WorkspaceKeys;
  readonly members: Members;
  readonly vault: Vault;
  #info: WorkspaceInfo | null = null;

  constructor(client: Tilo, ref: string | null) {
    this.client = client;
    this.ref = ref;
    this.files = new Files(this);
    this.keys = new WorkspaceKeys(this);
    this.members = new Members(this);
    this.vault = new Vault(this);
  }

  toString(): string {
    return `Workspace(${this.#info?.name || this.ref || ""})`;
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
      if (cursor !== null) {
        args.cursor = cursor;
      }
      return new ListingValue(await endpoint.call("tilo_ls", args), (next) => page(next));
    };
    return page(options.cursor ?? null);
  }

  /**
   * One resource's answer, as the host gives it: a record read as its first
   * record's fields, and any app's read with its surface's version. `surface`
   * is the app's version the types were made from, checked before the read.
   */
  async readRaw(
    path: string,
    options: { cursor?: string | null; surface?: string | null } = {},
  ): Promise<Record<string, unknown>> {
    const endpoint = await this.endpoint();
    return endpoint.read(path, options.cursor ?? null, options.surface ?? null);
  }

  /** A resource read whole: its fields, its children and its actions. */
  async read(path: string): Promise<Resource> {
    return (await this.resource(path).load()) as Resource;
  }

  /**
   * A resource as a typed object: fields, actions and field edits. `surface`
   * is the app's version the types were made from, which generated types
   * pass, and which every call through them checks.
   */
  resource(path: string, surface: Surface = null): Resource {
    return resourceProxy(this, path, surface);
  }

  /** Search the workspace by words, best first: its `search#find` action. */
  async find(words: string, app: string | null = null, limit: number | null = null): Promise<Hit[]> {
    const args: Record<string, unknown> = { query: words };
    if (app !== null) {
      args.app = app;
    }
    if (limit !== null) {
      args.limit = limit;
    }
    const found = recordOf((await this.endpoint()).action("search#find", args));
    const hits = found.hits;
    return Array.isArray(hits)
      ? hits
          .filter((hit): hit is Record<string, unknown> => hit !== null && typeof hit === "object")
          .map(hitOf)
      : [];
  }

  /**
   * One call in this workspace, `app/resource#action`: its result, the `Invocation`
   * that follows it, or `AskPending` when it waits for the person. `surface`
   * is the app's version the types were made from, for a call made through them.
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
    const page = async (cursor: string | null): Promise<InvocationList> => {
      // The workspace's invocations are a record, read whole through the record API; the page
      // is cut from it here, by where the cursor left off.
      const read = await this.readRaw("record:tilo.invocations:");
      const fields = read.fields;
      if (typeof fields !== "object" || fields === null) {
        throw new ProtocolError("the workspace answered no record of its invocations");
      }
      const listed = fields as Record<string, unknown>;
      const items = Array.isArray(listed.items) ? listed.items : [];
      const offset = cursor !== null && /^\d+$/.test(cursor) ? Number(cursor) : 0;
      const size = Math.max(1, Math.min(options.limit ?? 50, 200));
      const shown = items.slice(offset, offset + size);
      const end = offset + shown.length;
      const answer: Record<string, unknown> = {
        ...listed,
        items: shown,
      };
      if (end < items.length) {
        answer.next_cursor = String(end);
      } else {
        delete answer.next_cursor;
      }
      return new InvocationList(answer, (next) => page(next));
    };
    return page(options.cursor ?? null);
  }

  /**
   * Run a tilolang program as one step, in the program session `session` names (an earlier
   * answer's `session`), or a new one.
   */
  async program(source: string, session: string | null = null): Promise<ProgramAnswer> {
    return (await this.endpoint()).program(source, session);
  }

  /** Each change, invocation event and presence change, the cursor kept. */
  async *watch(path: string | null = null): AsyncGenerator<Event> {
    yield* (await this.endpoint()).events(path);
  }

  /**
   * The link to hand a person for what `path` names (`notes/Todo`): its
   * `share-link` action. It opens the record in its app for the workspace's
   * members and carries no secret: anyone else must be invited first. `mode`
   * is `app` (the app alone, the default) or `desktop`.
   */
  async link(path: string, options: { mode?: "app" | "desktop" | null } = {}): Promise<string> {
    const mode = options.mode ?? null;
    const result = await (await this.endpoint()).action(
      `${path}#share-link`,
      mode === null ? {} : { mode },
    );
    const url = recordOf(result).url;
    if (typeof url !== "string" || url === "") {
      throw new ProtocolError(`the workspace answered no link for ${path}`);
    }
    return url;
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
    return (await this.endpoint()).view(path, options.picture === true, options.scale ?? null);
  }

  /**
   * Everything the workspace holds, read whole: its listing (with its
   * labels) and every resource of every app, read by walking the listings
   * from its apps; the ones that did not read are in `unread`, with why. The
   * files the resources name (`tilo.content:` references) download with
   * `files.download`. Take one before deleting a customer's workspace.
   */
  async export(): Promise<WorkspaceExport> {
    const exportedAtMs = Date.now();
    const resources: Record<string, unknown>[] = [];
    const unread: { path: string; error: string }[] = [];
    const why = (error: unknown): string => (error instanceof Error ? error.message : String(error));
    const seen = new Set<string>();
    const walk: (string | null)[] = [null];
    while (walk.length > 0) {
      const path = walk.shift() ?? null;
      let listing: Listing;
      try {
        listing = await this.ls(path);
      } catch (error) {
        unread.push({ path: path ?? "", error: why(error) });
        continue;
      }
      for await (const row of listing) {
        if (seen.has(row.path)) {
          continue;
        }
        seen.add(row.path);
        if (row.raw.reads !== false) {
          try {
            resources.push(await this.readWhole(row.path));
          } catch (error) {
            unread.push({ path: row.path, error: why(error) });
          }
        }
        if (row.hasChildren) {
          walk.push(row.path);
        }
      }
    }
    return { workspace: await this.info(), exportedAtMs, resources, unread };
  }

  /** `path` read with every page of what it holds. */
  private async readWhole(path: string): Promise<Record<string, unknown>> {
    const children: unknown[] = [];
    let whole: Record<string, unknown> = {};
    let cursor: string | null = null;
    for (;;) {
      const page: Record<string, unknown> = await this.readRaw(path, { cursor });
      if (cursor === null) {
        whole = { ...page };
      }
      children.push(...(Array.isArray(page.children) ? page.children : []));
      whole.children = children;
      const following = page.cursor;
      if (typeof following !== "string" || following === cursor) {
        delete whole.cursor;
        return whole;
      }
      cursor = following;
    }
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
    const result = await this.client
      .account()
      .action("account/workspaces#unattended", { workspace: await this.id(), on });
    return result instanceof Invocation ? {} : recordOf(result);
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
