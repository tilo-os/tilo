/** The client: the account's workspaces and keys, and where every call goes. */

import { AskPending, ProtocolError, Refused, refusalOf } from "./errors.ts";
import { DEFAULT_URL, Endpoint, RETRY_DEADLINE_MS } from "./endpoint.ts";
import {
  type ApiInfo,
  apiInfoOf,
  type Key,
  keyOf,
  type MintedKey,
  mintedKeyOf,
  type Usage,
  usageOf,
  type WorkspaceInfo,
  WorkspaceList,
  workspaceOf,
} from "./models.ts";
import { routes } from "./routes.ts";
import {
  type FetchLike,
  type HostOptions,
  HttpTransport,
  NativeTransport,
  type Transport,
} from "./transport.ts";
import { Workspace } from "./workspace.ts";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** How a client is made: where it calls, with which key, and how long it retries. */
export interface TiloOptions {
  /** The API server: `TILO_API_URL` when not given, else `https://api.tilo.sh`. */
  url?: string | null;
  /** The key: `TILO_API_KEY` when not given. */
  key?: string | null;
  /** How it reaches the API: `fetch` unless you bring your own. */
  fetch?: FetchLike;
  /**
   * How long a call keeps asking again while the host is busy with the
   * person's changes or unreachable, in ms (60 000 by default; 0 throws
   * `RateLimited` at once).
   */
  retryDeadlineMs?: number;
  /** The seam under the API: an API server over `fetch` unless you pass one. */
  transport?: Transport;
}

/** The API server a call goes to: the one named, else `TILO_API_URL`, else the default. */
export function urlFrom(url?: string | null): string {
  return (url ?? process.env.TILO_API_URL ?? DEFAULT_URL).replace(/\/+$/, "");
}

/** The key a call goes with: the one named, else `TILO_API_KEY`. */
export function keyFrom(key?: string | null): string | null {
  return key ?? process.env.TILO_API_KEY ?? null;
}

/** A key being approved: the link to show, and the wait for it. */
export class SignIn {
  readonly client: Tilo;
  readonly url: string;
  readonly poll: string;
  readonly intervalSeconds: number;
  readonly expiresInSeconds: number;

  constructor(
    client: Tilo,
    url: string,
    poll: string,
    intervalSeconds: number,
    expiresInSeconds: number,
  ) {
    this.client = client;
    this.url = url;
    this.poll = poll;
    this.intervalSeconds = intervalSeconds;
    this.expiresInSeconds = expiresInSeconds;
  }

  /** Follow the approval until the person decides, then the `Key`. */
  async wait(): Promise<Key> {
    const deadline = Date.now() + this.expiresInSeconds * 1000;
    for (;;) {
      const reply = await this.client
        .account()
        .request("POST", routes.pollKey(), { json: { poll: this.poll }, idempotent: false });
      if (reply.status === 200) {
        return keyOf(reply.body ?? {});
      }
      if (reply.status !== 202) {
        throw refusalOf(reply.status, reply.body);
      }
      if (Date.now() >= deadline) {
        throw new Refused("expired", "nothing approved the key in time");
      }
      await new Promise((resolve) => setTimeout(resolve, this.intervalSeconds * 1000));
    }
  }
}

/** The person's workspaces: list them, make one, delete one. */
export class Workspaces {
  readonly client: Tilo;

  constructor(client: Tilo) {
    this.client = client;
  }

  /** Every workspace this key reaches, wherever it is hosted. */
  list(): Promise<WorkspaceList> {
    return this.client.workspaceList(true);
  }

  /** Create a workspace by name. */
  create(name: string): Promise<WorkspaceInfo> {
    return this.client.createWorkspace(name);
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
   * time; submitting a page still asks them. Throws `AskPending` with the
   * link they approve it at; `await ask.wait()` answers the `Key` once they
   * do.
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
 * Every call is one of Tilo's `/v1` routes (`docs/developers/api.md`), with
 * the key as `Authorization: Bearer`; a host in process serves the same
 * routes (`Tilo.host(…)`).
 */
export class Tilo {
  readonly transport: Transport;
  readonly workspaces: Workspaces;
  readonly keys: Keys;
  readonly retryDeadlineMs: number;
  catalog: WorkspaceList | null = null;
  readonly #streams = new AbortController();

  constructor(options: TiloOptions = {}) {
    this.retryDeadlineMs = options.retryDeadlineMs ?? RETRY_DEADLINE_MS;
    this.transport =
      options.transport ??
      new HttpTransport(
        urlFrom(options.url),
        keyFrom(options.key),
        options.fetch ?? globalThis.fetch,
      );
    this.workspaces = new Workspaces(this);
    this.keys = new Keys(this);
  }

  /**
   * A Tilo host in process, as one of the person's devices: the same `/v1`
   * routes without a socket, through the native addon. Its key is the file
   * the host keeps, or the key and certificate given.
   *
   * ```ts
   * const tilo = await Tilo.host("https://eu1.tilo.sh", { keyFile: "~/.local/state/tilo/key" });
   * ```
   */
  static async host(origin: string, options: HostOptions = {}): Promise<Tilo> {
    return new Tilo({
      url: origin,
      transport: await NativeTransport.open(origin, options),
    });
  }

  /** One workspace, by its id or by its name. */
  workspace(idOrName: string): Workspace {
    return new Workspace(this, idOrName);
  }

  /** @internal The account's calls, or one workspace's. */
  account(): Endpoint {
    return new Endpoint(this.transport, null, this.retryDeadlineMs, this.#streams.signal);
  }

  /** @internal One workspace's calls. */
  at(workspace: string): Endpoint {
    return new Endpoint(this.transport, workspace, this.retryDeadlineMs, this.#streams.signal);
  }

  /** The API's version and build, and what this key reaches. */
  async info(): Promise<ApiInfo> {
    return apiInfoOf(await this.account().info());
  }

  /** The person's plan, and per workspace what it keeps and the VM time it ran. */
  async usage(): Promise<Usage> {
    return usageOf(await this.account().answer("GET", routes.account(), { idempotent: false }));
  }

  /**
   * A key this machine does not have yet: show the link, wait for approval.
   * It reaches every workspace unless `workspace` names one, and lasts until
   * revoked unless `expiresIn` (seconds) says otherwise.
   */
  async signIn(
    name: string,
    workspace: string | null = null,
    expiresIn: number | null = null,
    browserPages = true,
  ): Promise<SignIn> {
    const json: Record<string, unknown> = { name };
    if (workspace !== null) {
      json.workspace = await this.workspaceId(workspace);
    }
    if (expiresIn !== null) {
      json.expires_in = expiresIn;
    }
    if (!browserPages) {
      json.browser_pages = false;
    }
    const answer = await this.account().answer("POST", routes.newKey(), {
      json,
      idempotent: false,
    });
    return new SignIn(
      this,
      String(answer.approve_url ?? ""),
      String(answer.poll ?? ""),
      Number(answer.interval ?? 2),
      Number(answer.expires_in ?? 300),
    );
  }

  /** End the streams this client opened; the calls it made are done already. */
  close(): void {
    this.#streams.abort();
  }

  /** @internal */
  async workspaceList(force = false): Promise<WorkspaceList> {
    if (!force && this.catalog !== null) {
      return this.catalog;
    }
    this.catalog = new WorkspaceList(
      await this.account().answer("GET", routes.workspaces(), { idempotent: false }),
    );
    return this.catalog;
  }

  /** @internal */
  async workspaceInfo(ref: string): Promise<WorkspaceInfo> {
    let listing: WorkspaceList;
    try {
      listing = await this.workspaceList();
    } catch (error) {
      if ((error instanceof Refused || error instanceof ProtocolError) && UUID.test(ref)) {
        return { id: ref, name: "", host: "", role: "" };
      }
      throw error;
    }
    const found = listing.named(ref);
    // A workspace the listing does not name still answers to its id, as the
    // host decides.
    if (found === undefined && UUID.test(ref)) {
      return { id: ref, name: "", host: "", role: "" };
    }
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

  /** @internal */
  async createWorkspace(name: string): Promise<WorkspaceInfo> {
    this.catalog = null;
    return workspaceOf(
      await this.account().answer("POST", routes.workspaces(), { json: { name } }),
    );
  }

  /** @internal */
  async deleteWorkspace(workspace: string | Workspace): Promise<Record<string, unknown>> {
    const id = await this.workspaceId(workspace);
    this.catalog = null;
    return this.account().answer("DELETE", routes.workspace(id));
  }

  /** @internal */
  async keysList(): Promise<Key[]> {
    const listed = await this.account().answer("GET", routes.keys(), { idempotent: false });
    return Array.isArray(listed.keys)
      ? listed.keys
          .filter((key): key is Record<string, unknown> => key !== null && typeof key === "object")
          .map(keyOf)
      : [];
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
    return mintedKeyOf(await this.account().answer("POST", routes.keys(), { json }));
  }

  /** @internal */
  async revokeKey(grant: string): Promise<Record<string, unknown>> {
    return this.account().answer("DELETE", routes.key(grant));
  }

  /** @internal A key the person approves on their own device: `AskPending` until they do. */
  async keyCreate(
    name: string,
    workspace: string | Workspace | null,
    expiresIn: number | null = null,
    browserPages = true,
  ): Promise<Key> {
    const signIn = await this.signIn(
      name,
      workspace === null ? null : await this.workspaceId(workspace),
      expiresIn,
      browserPages,
    );
    throw new AskPending(signIn.url, idOf(signIn.url), { name }, async () => {
      return (await signIn.wait()) as unknown as Record<string, unknown>;
    });
  }
}

/** The last segment of an approval link: the id of the ask it names. */
function idOf(url: string): string {
  const parts = url.split("/").filter((part) => part !== "");
  return parts[parts.length - 1] ?? "";
}
