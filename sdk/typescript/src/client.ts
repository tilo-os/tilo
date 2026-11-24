/** The client: the account's workspaces and keys, and where every call goes. */

import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex } from "@noble/hashes/utils.js";

import { DaemonTransport, KeyTransport, type Transport, type FetchLike } from "./transport.ts";
import { Endpoint } from "./endpoint.ts";
import { AskPending, ProtocolError, Refused } from "./errors.ts";
import { Invocation } from "./invocation.ts";
import {
  type Key,
  keyGranted,
  keyOf,
  type MintedKey,
  mintedKeyOf,
  arrayOf,
  recordOf,
  type Usage,
  usageOf,
  type WorkspaceInfo,
  WorkspaceList,
  workspaceOf,
} from "./models.ts";
import { Workspace } from "./workspace.ts";

/** The hosted server a key reaches when none is named. */
export const DEFAULT_URL = "https://api.tilo.sh";
/** How long a key's call keeps asking again while the server is busy, in ms. */
export const RETRY_DEADLINE_MS = 60_000;

/**
 * The keys a workspace admitted, each a record of its own: listing it names the grants this key
 * may read, and each is read as every record is. No action lists them.
 */
const KEYS_RECORD = "record:tilo.grants";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** How a client is made: how it reaches Tilo, and how long it retries. */
export interface TiloOptions {
  /** A key: `TILO_API_KEY` when not given. With one, calls go to the hosted server. */
  key?: string | null;
  /** The hosted server: `TILO_API_URL` when not given, else `https://api.tilo.sh`. */
  url?: string | null;
  /** The daemon's native socket: this computer's daemon when not given. */
  socket?: string | null;
  /** The workspace an app's connection acts in, when the daemon serves more than one. */
  workspace?: string | null;
  /** The boundary under the SDK: the daemon, or a key's server, unless you bring one. */
  transport?: Transport;
  /** How the key transport reaches the server: `fetch` unless you bring your own. */
  fetch?: FetchLike;
  /**
   * How long a key's call keeps asking again while the server is briefly
   * unavailable or unreachable, in ms (60 000 by default; 0 throws its
   * refusal at once).
   */
  retryDeadlineMs?: number;
}

/** The hosted server a call goes to: the one named, else `TILO_API_URL`, else the default. */
export function urlFrom(url?: string | null): string {
  return (url ?? process.env.TILO_API_URL ?? DEFAULT_URL).replace(/\/+$/, "");
}

/** The key a call goes with: the one named, else `TILO_API_KEY`. */
export function keyFrom(key?: string | null): string | null {
  return key ?? process.env.TILO_API_KEY ?? null;
}

/** The person's workspaces: list them, make one, delete one. */
export class Workspaces {
  readonly client: Tilo;

  constructor(client: Tilo) {
    this.client = client;
  }

  /**
   * Every workspace this key reaches, wherever it is hosted; with `labels`,
   * only those labelled with every one of them.
   *
   * ```ts
   * const theirs = await tilo.workspaces.list({ labels: { customer_id: "cus_8121" } });
   * ```
   */
  list(options: { labels?: Record<string, string> } = {}): Promise<WorkspaceList> {
    const labels = options.labels ?? {};
    return Object.keys(labels).length === 0
      ? this.client.workspaceList(true)
      : this.client.labelledWorkspaces(labels);
  }

  /**
   * Create a workspace by name, labelled for your programs to find it by
   * (`{ customer_id: "cus_8121" }`: at most 16, read by your keys alone).
   * The same `idempotencyKey` (your customer's id, say) answers the
   * workspace the first create with it made, however often it is asked
   * again, so a backend that crashed mid-create never makes two.
   */
  create(
    name: string,
    options: { labels?: Record<string, string>; idempotencyKey?: string } = {},
  ): Promise<WorkspaceInfo> {
    return this.client.createWorkspace(name, options.labels ?? {}, options.idempotencyKey ?? null);
  }

  /**
   * Branch `source`, a workspace of the person's own, into a new one named
   * `name`. The branch starts with everything `source` holds, its records
   * with their history and its files, and goes on apart from it: nothing
   * merges back. Its chat, its members and each person's own places stay
   * behind; the person is the branch's only member. `source` is an id, a
   * name, a `Workspace` or what `create` answered. `labels` and
   * `idempotencyKey` are as `create` takes them; the source's labels do not
   * come along.
   *
   * ```ts
   * const acme = await tilo.workspaces.branch(template, "Acme", { labels: { customer_id: "cus_8121" } });
   * ```
   */
  branch(
    source: string | Workspace | WorkspaceInfo,
    name: string,
    options: { labels?: Record<string, string>; idempotencyKey?: string } = {},
  ): Promise<WorkspaceInfo> {
    return this.client.branchWorkspace(
      source,
      name,
      options.labels ?? {},
      options.idempotencyKey ?? null,
    );
  }

  /**
   * The one workspace labelled with every one of `labels`, or `undefined`
   * when none is; `Refused("conflict")` when more than one is.
   */
  async find(labels: Record<string, string>): Promise<WorkspaceInfo | undefined> {
    const found = (await this.client.labelledWorkspaces(labels)).workspaces;
    if (found.length > 1) {
      throw new Refused("conflict", `${found.length} workspaces are labelled so`, [
        "await tilo.workspaces.list({ labels }) names them",
      ]);
    }
    return found[0];
  }

  /**
   * Delete a workspace of the person's own, and everything in it: final.
   * Its records, files and history go from the host; anyone in it is told it
   * was deleted, and every key minted for it is revoked. Answers
   * `{deleted, revoked_keys}`. Only a key reaching all the person's
   * workspaces deletes; a workspace they were invited into is `not_found`, as
   * is one already deleted. The call is the confirmation: nothing asks again.
   */
  delete(workspace: string | Workspace): Promise<Record<string, unknown>> {
    return this.client.deleteWorkspace(workspace);
  }

  /** The workspace an id or name names, from the last listing. */
  named(idOrName: string): WorkspaceInfo | undefined {
    return this.client.catalog?.named(idOrName);
  }
}

/** The account's keys: this key, the keys it minted, and new ones. */
export class Keys {
  readonly client: Tilo;

  constructor(client: Tilo) {
    this.client = client;
  }

  /** This key and the keys it minted. */
  list(): Promise<Key[]> {
    return this.client.keysList();
  }

  /**
   * Ask the person for a new key.
   *
   * A key reaching all their workspaces lasts until revoked unless
   * `expiresIn` (seconds) says when it expires; one reaching a single
   * workspace lasts at most 89 days. `browserPages` (on unless you say no)
   * lets the key operate web pages in its workspace's Browser — navigating,
   * filling in, selecting and clicking — without asking the person each
   * time; submitting a page still asks them. Throws `AskPending` while they
   * have not decided; `await ask.wait()` answers the `Key` once they do.
   */
  async create(
    name: string,
    workspace: string | Workspace | null = null,
    expiresIn: number | null = null,
    browserPages = true,
  ): Promise<Key> {
    return this.client.keyCreate(name, workspace, expiresIn, browserPages);
  }

  /** A key for one workspace, for an agent there; narrower than your own. */
  mint(
    workspace: string | Workspace,
    name: string | null = null,
    expiresIn: number | null = null,
  ): Promise<MintedKey> {
    return this.client.mintKey(workspace, name, expiresIn);
  }

  /** Revoke a key and every key minted from it. */
  revoke(grant: string): Promise<Record<string, unknown>> {
    return this.client.revokeKey(grant);
  }
}

/**
 * The account: `await tilo.workspaces.list()`, `await ws.do(...)`.
 *
 * Every call is one tool of Tilo's agent surface, carried by this computer's
 * daemon through the native addon, or by a key to the hosted server
 * (`new Tilo({ key })`). Nothing speaks `/v1`.
 */
export class Tilo {
  readonly transport: Transport;
  readonly workspaces: Workspaces;
  readonly keys: Keys;
  readonly retryDeadlineMs: number;
  /** The surfaces this client has checked, by `${workspace}:${app}`. */
  readonly surfaces = new Map<string, string>();
  catalog: WorkspaceList | null = null;
  readonly #streams = new AbortController();

  constructor(options: TiloOptions = {}) {
    this.retryDeadlineMs = options.retryDeadlineMs ?? RETRY_DEADLINE_MS;
    const key = keyFrom(options.key);
    this.transport =
      options.transport ??
      (key !== null
        ? new KeyTransport(urlFrom(options.url), key, {
            fetch: options.fetch ?? globalThis.fetch,
            retryDeadlineMs: this.retryDeadlineMs,
            signal: this.#streams.signal,
          })
        : new DaemonTransport(options.socket ?? null, options.workspace ?? null));
    this.workspaces = new Workspaces(this);
    this.keys = new Keys(this);
  }

  /**
   * One workspace, by its id or by its name; with none, the one the
   * daemon's connection acts in, which is how an app finds its workspace.
   */
  workspace(ref: string | null = null): Workspace {
    return new Workspace(this, ref);
  }

  /** @internal The account's calls, or one workspace's. */
  account(): Endpoint {
    return new Endpoint(this.transport, null, this.surfaces, this.#streams.signal);
  }

  /** @internal One workspace's calls. */
  at(workspace: string): Endpoint {
    return new Endpoint(this.transport, workspace, this.surfaces, this.#streams.signal);
  }

  /** The person's plan, and per workspace what it keeps and the VM time it ran. */
  async usage(): Promise<Usage> {
    const read = await this.account().read("account/usage");
    return usageOf(recordOf(read.fields));
  }

  /** End the watches this client opened; the calls it made are done already. */
  close(): void {
    this.#streams.abort();
  }

  /** @internal */
  async workspaceList(force = false): Promise<WorkspaceList> {
    if (!force && this.catalog !== null) {
      return this.catalog;
    }
    this.catalog = new WorkspaceList(await this.accountAction("account/workspaces#list"));
    return this.catalog;
  }

  /**
   * @internal An action of the account (`account/workspaces#create`), as every action is run:
   * what it answered. `idempotencyKey` names the invocation, so a retry with the same one runs
   * once.
   */
  async accountAction(
    path: string,
    json: Record<string, unknown> = {},
    idempotencyKey: string | null = null,
  ): Promise<Record<string, unknown>> {
    const invocationId = idempotencyKey === null ? null : invocationIdOf(idempotencyKey);
    const result = await this.account().action(path, json, null, invocationId);
    const settled = result instanceof Invocation ? await result.result() : result;
    return recordOf(settled);
  }

  /** @internal The workspace an action of the account answered. */
  private static made(answer: Record<string, unknown>): WorkspaceInfo {
    return workspaceOf(recordOf(answer.workspace));
  }

  /**
   * @internal The workspace `ref` names: one the daemon serves, a workspace's id, or a name the
   * account lists.
   */
  async workspaceInfo(ref: string | null): Promise<WorkspaceInfo> {
    // An id needs no listing — and an app may not list the account's workspaces.
    if (ref !== null && UUID.test(ref)) {
      return blank(ref);
    }
    const served = await this.transport.workspace();
    if (ref === null) {
      if (served === null) {
        throw new ProtocolError("name a workspace: this client's transport serves none");
      }
      return blank(served);
    }
    if (ref === served) {
      return blank(ref);
    }
    const found = (await this.workspaceList()).named(ref);
    if (found === undefined) {
      throw new Refused("not_found", `no workspace ${ref}`, [
        "await tilo.workspaces.list() names them",
      ]);
    }
    return found;
  }

  /** The id of a workspace named by it, by its name, or as a `Workspace`. */
  async workspaceId(workspace: string | Workspace): Promise<string> {
    if (typeof workspace === "string") {
      return UUID.test(workspace) ? workspace : (await this.workspaceInfo(workspace)).id;
    }
    return workspace.id();
  }

  /** @internal The workspaces labelled with every one of `labels`. */
  async labelledWorkspaces(labels: Record<string, string>): Promise<WorkspaceList> {
    return new WorkspaceList(await this.accountAction("account/workspaces#list", { labels }));
  }

  /** @internal */
  async createWorkspace(
    name: string,
    labels: Record<string, string> = {},
    idempotencyKey: string | null = null,
  ): Promise<WorkspaceInfo> {
    this.catalog = null;
    const json: Record<string, unknown> = { name };
    if (Object.keys(labels).length > 0) {
      json.labels = labels;
    }
    return Tilo.made(await this.accountAction("account/workspaces#create", json, idempotencyKey));
  }

  /** @internal */
  async branchWorkspace(
    source: string | Workspace | WorkspaceInfo,
    name: string,
    labels: Record<string, string> = {},
    idempotencyKey: string | null = null,
  ): Promise<WorkspaceInfo> {
    const id =
      typeof source === "string" || source instanceof Workspace
        ? await this.workspaceId(source)
        : source.id;
    this.catalog = null;
    const json: Record<string, unknown> = { workspace: id, name };
    if (Object.keys(labels).length > 0) {
      json.labels = labels;
    }
    return Tilo.made(await this.accountAction("account/workspaces#branch", json, idempotencyKey));
  }

  /** @internal */
  async deleteWorkspace(workspace: string | Workspace): Promise<Record<string, unknown>> {
    const id = await this.workspaceId(workspace);
    this.catalog = null;
    return this.accountAction("account/workspaces#delete", { workspace: id });
  }

  /**
   * @internal The keys each workspace admitted: its listing names the ones this key may read,
   * each followed to its grant. A key reaching every workspace is in each of them.
   */
  async keysList(): Promise<Key[]> {
    const keys = new Map<string, Key>();
    const seen = new Set<string>();
    for (const workspace of (await this.workspaceList(true)).workspaces) {
      const endpoint = this.at(workspace.id);
      const listed = await endpoint.call("tilo_ls", { path: KEYS_RECORD });
      for (const row of arrayOf(listed.rows)) {
        const path = String(row.path ?? "");
        // A record is named by its grant: one read in another workspace is read already.
        if (path === "" || seen.has(path)) {
          continue;
        }
        seen.add(path);
        const grant = recordOf((await endpoint.read(path)).fields);
        if (grant.revoked_ms == null && Object.keys(grant).length > 0) {
          const key = keyGranted(grant);
          keys.set(key.grant, key);
        }
      }
    }
    return [...keys.values()];
  }

  /** @internal */
  async mintKey(
    workspace: string | Workspace,
    name: string | null,
    expiresIn: number | null,
  ): Promise<MintedKey> {
    const json: Record<string, unknown> = { workspace: await this.workspaceId(workspace) };
    if (name !== null) {
      json.name = name;
    }
    if (expiresIn !== null) {
      json.expires_in = expiresIn;
    }
    return mintedKeyOf(await this.accountAction("account/keys#mint", json));
  }

  /** @internal */
  async revokeKey(grant: string): Promise<Record<string, unknown>> {
    return this.accountAction(`account/keys/${grant}#revoke`);
  }

  /** @internal A key the person approves on their own device: `AskPending` until they do. */
  async keyCreate(
    name: string,
    workspace: string | Workspace | null,
    expiresIn: number | null = null,
    browserPages = true,
  ): Promise<Key> {
    const json: Record<string, unknown> = { name, browser_pages: browserPages };
    if (workspace !== null) {
      json.workspace = await this.workspaceId(workspace);
    }
    if (expiresIn !== null) {
      json.expires_in = expiresIn;
    }
    const result = await this.account().action("account/keys#create", json);
    // The person approves a new key on a device of their own: a call still
    // pending is one waiting for them, not a key.
    if (result instanceof Invocation) {
      throw new AskPending(null, result.invocationId, {}, async () => {
        const key = keyOf(recordOf(await result.result()));
        return key as unknown as Record<string, unknown>;
      });
    }
    return keyOf(recordOf(result));
  }
}

/** A workspace named by its id alone: its name and host are not read. */
function blank(id: string): WorkspaceInfo {
  return { id, name: "", host: "", role: "", labels: {}, unattended: false };
}

/**
 * The invocation a caller's idempotency key names: the same key is the same
 * invocation, so a retry runs once. A key already written as a UUID is one;
 * any other is hashed into the UUID the tools name an invocation by.
 */
function invocationIdOf(idempotencyKey: string): string {
  if (UUID.test(idempotencyKey)) {
    return idempotencyKey;
  }
  const hex = bytesToHex(sha256(new TextEncoder().encode(`tilo-sdk/${idempotencyKey}`))).slice(0, 32);
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20, 32)}`;
}
