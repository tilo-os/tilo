/** The client: the account's workspaces and keys, and the door it all goes to. */

import { AskPending, ProtocolError, Refused } from "./errors.ts";
import { Link } from "./link.ts";
import {
  keyOf,
  type Key,
  mintedKeyOf,
  type MintedKey,
  type WorkspaceInfo,
  WorkspaceList,
} from "./models.ts";
import { Transport, type Answer, type FetchLike } from "./wire.ts";
import { Workspace } from "./workspace.ts";

const ACCOUNT = "account";

export interface TiloOptions {
  key?: string | null;
  door?: string | null;
  fetch?: FetchLike;
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
      const response = await this.client.transport.request("POST", "/keys/poll", {
        body: { poll: this.poll },
      });
      const body = await jsonOf(response);
      if (response.status === 200) {
        return keyOf(body);
      }
      if (response.status === 202) {
        if (Date.now() >= deadline) {
          throw new Refused("expired", "nothing approved the key in time");
        }
        await new Promise((resolve) => setTimeout(resolve, this.intervalSeconds * 1000));
        continue;
      }
      throw refusedFrom(body, response.status);
    }
  }
}

async function jsonOf(response: Response): Promise<Record<string, unknown>> {
  try {
    const body = await response.clone().json();
    return body && typeof body === "object" ? (body as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

function refusedFrom(body: Record<string, unknown>, status: number): Error {
  const error = body.error;
  if (error && typeof error === "object") {
    const detail = error as Record<string, unknown>;
    return new Refused(
      String(detail.code ?? "failed"),
      String(detail.message ?? ""),
      Array.isArray(detail.next_steps) ? detail.next_steps.map(String) : [],
    );
  }
  return new ProtocolError(`the door answered ${status}`, status);
}

/** The person's workspaces: list them, make one, delete one. */
export class Workspaces {
  readonly client: Tilo;

  constructor(client: Tilo) {
    this.client = client;
  }

  /** Every workspace this key reaches, and the hosts to create one on. */
  list(): Promise<WorkspaceList> {
    return this.client.workspaceList();
  }

  /** Create a workspace by name, on `host` (the default host without one). */
  async create(name: string, host: string | null = null): Promise<WorkspaceInfo> {
    return this.client.createWorkspace(name, host);
  }

  /** Delete a workspace: refused today, the host cannot yet. */
  async delete(workspace: string | Workspace): Promise<unknown> {
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
   * workspace lasts at most 89 days. Throws `AskPending` with the link they
   * approve it at; `await ask.wait()` answers the `Key` once they do.
   */
  create(
    name: string,
    workspace: string | Workspace | null = null,
    expiresIn: number | null = null,
  ): Promise<Key> {
    return this.client.keyCreate(name, workspace, expiresIn);
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
  revoke(grant: string): Promise<unknown> {
    return this.client.revokeKey(grant);
  }
}

function looksLikeId(ref: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(ref);
}

/**
 * The account: `await tilo.workspaces.list()`, `await ws.do(...)`.
 *
 * Every call is the `tools/call` an agent makes, asking for typed answers.
 */
export class Tilo {
  readonly transport: Transport;
  readonly workspaces: Workspaces;
  readonly keys: Keys;
  catalog: WorkspaceList | null = null;

  constructor(options: TiloOptions = {}) {
    this.transport = new Transport(options);
    this.workspaces = new Workspaces(this);
    this.keys = new Keys(this);
  }

  /** One workspace, by its id or by its name. */
  workspace(idOrName: string): Workspace {
    return new Workspace(this, idOrName);
  }

  /**
   * A key this machine does not have yet: show the link, wait for approval.
   * It lasts until revoked unless `expiresIn` (seconds) says otherwise.
   */
  async signIn(
    name: string | null = null,
    workspace: string | null = null,
    expiresIn: number | null = null,
  ): Promise<SignIn> {
    const body: Record<string, unknown> = {};
    if (name !== null) {
      body.name = name;
    }
    if (workspace !== null) {
      body.workspace = workspace;
    }
    if (expiresIn !== null) {
      body.expires_in = expiresIn;
    }
    const response = await this.transport.request("POST", "/keys/new", { body });
    const answer = await jsonOf(response);
    if (response.status >= 400) {
      throw refusedFrom(answer, response.status);
    }
    return new SignIn(
      this,
      String(answer.approve_url ?? ""),
      String(answer.poll ?? ""),
      Number(answer.interval ?? 2),
      Number(answer.expires_in ?? 300),
    );
  }

  /** End every session this client has open. */
  close(): Promise<void> {
    return this.transport.close();
  }

  link(): Link {
    return new Link(this.transport, null);
  }

  async workspaceList(): Promise<WorkspaceList> {
    const result = await this.link().outcome(`${ACCOUNT}/workspaces#list`, {});
    this.catalog = new WorkspaceList(
      result && typeof result === "object" ? (result as Record<string, unknown>) : {},
    );
    return this.catalog;
  }

  async workspaceInfo(ref: string): Promise<WorkspaceInfo> {
    let listing: WorkspaceList;
    try {
      listing = await this.workspaceList();
    } catch (error) {
      if (error instanceof Refused || error instanceof ProtocolError) {
        if (looksLikeId(ref)) {
          return { id: ref, name: "", host: "", role: "" };
        }
      }
      throw error;
    }
    const found = listing.named(ref);
    // A workspace the door does not name still answers to its id, as the door
    // decides.
    if (found === undefined && looksLikeId(ref)) {
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
      return looksLikeId(workspace) ? workspace : (await this.workspaceInfo(workspace)).id;
    }
    return workspace.id();
  }

  async createWorkspace(name: string, host: string | null): Promise<WorkspaceInfo> {
    const args: Record<string, unknown> = { name };
    if (host !== null) {
      args.host = host;
    }
    const result = await this.link().outcome(`${ACCOUNT}/workspaces#create`, args);
    this.catalog = null;
    const created = (result as Record<string, unknown> | null)?.workspace;
    if (!created || typeof created !== "object") {
      throw new ProtocolError("the door created a workspace without naming it");
    }
    const row = created as Record<string, unknown>;
    return {
      id: String(row.id ?? ""),
      name: String(row.name ?? ""),
      host: String(row.host ?? ""),
      role: String(row.role ?? ""),
    };
  }

  async deleteWorkspace(workspace: string | Workspace): Promise<unknown> {
    const result = await this.link().outcome(`${ACCOUNT}/workspaces#delete`, {
      workspace: await this.workspaceId(workspace),
    });
    this.catalog = null;
    return result;
  }

  async keysList(): Promise<Key[]> {
    const result = await this.link().outcome(`${ACCOUNT}/keys#list`, {});
    const keys = (result as Record<string, unknown> | null)?.keys;
    return Array.isArray(keys)
      ? keys.map((key) => keyOf(key as Record<string, unknown>))
      : [];
  }

  async mintKey(
    workspace: string | Workspace,
    name: string | null,
    expiresIn: number | null,
  ): Promise<MintedKey> {
    const args: Record<string, unknown> = { workspace: await this.workspaceId(workspace) };
    if (name !== null) {
      args.name = name;
    }
    if (expiresIn !== null) {
      args.expires_in = expiresIn;
    }
    const result = await this.link().outcome(`${ACCOUNT}/keys#mint`, args);
    if (!result || typeof result !== "object") {
      throw new ProtocolError("the door minted a key without naming it");
    }
    return mintedKeyOf(result as Record<string, unknown>);
  }

  async revokeKey(grant: string): Promise<unknown> {
    return this.link().outcome(`${ACCOUNT}/keys#revoke`, { grant });
  }

  async keyCreate(
    name: string,
    workspace: string | Workspace | null,
    expiresIn: number | null = null,
  ): Promise<Key> {
    const args: Record<string, unknown> = { name };
    if (workspace !== null) {
      args.workspace = await this.workspaceId(workspace);
    }
    if (expiresIn !== null) {
      args.expires_in = expiresIn;
    }
    try {
      await this.link().call("tilo_do", { path: `${ACCOUNT}/keys#create`, args });
    } catch (error) {
      if (error instanceof AskPending) {
        throw error.then((answer) =>
          keyOf((answer.result as Record<string, unknown>) ?? {}),
        );
      }
      throw error;
    }
    throw new ProtocolError("the door made a key without asking the person");
  }

  /** The typed answer as it is: for callers who want `tilo_ls` on the account. */
  async accountCall(tool: string, args: Record<string, unknown>): Promise<Answer> {
    return this.link().call(tool, args);
  }
}
