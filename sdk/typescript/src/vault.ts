/**
 * What only a daemon's connection carries besides the API: the vault's lane, which uses a secret
 * without handing it over, and an app's call and presence lanes.
 */

import { run, type Workspace } from "./client.ts";
import { Refused, refusalOf } from "./errors.ts";
import {
  Daemon,
  type CallAsk,
  type CallFrame,
  type CallReport,
  type CarriedCall,
  type DaemonConnection,
  type PresenceMember,
  type PresenceSaid,
} from "./transport.ts";

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

/** The daemon of `workspace`'s client, or the refusal of a lane only a daemon's connection has. */
function daemonOf(workspace: Workspace, what: string): Daemon {
  const daemon = workspace.client.daemon;
  if (daemon === null) {
    throw new Refused({
      code: "not_permitted",
      message: `${what} only an app this computer's daemon started, over the connection it was handed`,
    });
  }
  return daemon;
}

/** What `read` answers on the addon, which throws a refusal's body, as JSON, as its message. */
async function answered<T>(read: () => T | Promise<T>): Promise<T> {
  try {
    return await read();
  } catch (error) {
    let body: { error?: unknown };
    try {
      body = JSON.parse(error instanceof Error ? error.message : String(error)) as { error?: unknown };
    } catch {
      throw error;
    }
    throw refusalOf(body.error === undefined ? { error: body } : body);
  }
}

/**
 * The person's vault: a secret stays there, and a call uses it by reference. `webCall` has the
 * vault add the key on the way out and answers the service's reply with the key struck from it;
 * `ask` is the one way a program comes by an item it was not given. `find` names the item a
 * secret an app was moved to was kept as, and `release` hands that item's secret to the app's
 * own process.
 */
export class Vault {
  /** The workspace whose vault it is. */
  readonly workspace: Workspace;

  constructor(workspace: Workspace) {
    this.workspace = workspace;
  }

  /** Make a web request with a key the vault keeps, by reference. */
  async webCall(call: WebCallOptions): Promise<WebCallAnswer> {
    const args = Object.fromEntries(Object.entries(call).filter(([, value]) => value !== undefined));
    const daemon = this.workspace.client.daemon;
    const answer = (
      daemon === null
        ? (await run(this.workspace, { path: "vault/vault", action: "web-call" }, args)).result
        : (Daemon.answer(await (await daemon.opened()).webCall(await this.workspace.id(), args)) as { result?: unknown })
            .result
    ) as Partial<WebCallAnswer> | undefined;
    return {
      status: answer?.status ?? 0,
      headers: Object.fromEntries(Object.entries(answer?.headers ?? {}).map(([name, value]) => [name, String(value)])),
      body: answer?.body ?? "",
    };
  }

  /**
   * The secret of the vault item `item` (`vault.item:…`), handed to this app's process: only to
   * the process the daemon launched for an app's connection, for an item the workspace granted
   * it. Anything else throws `Refused`, and its person is asked once whether it may.
   */
  async release(item: string): Promise<string> {
    const daemon = daemonOf(this.workspace, "a vault item's secret is released to");
    const answer = Daemon.answer(await (await daemon.opened()).release(await this.workspace.id(), item));
    const secret = (answer as { secret?: unknown } | undefined)?.secret;
    if (typeof secret !== "string") {
      throw new Refused({ code: "failed", message: "the vault answered a release with no secret" });
    }
    return secret;
  }

  /**
   * Ask the person for a password, a key or another secret, which they type into the system's
   * protected field on their own device: the reference the vault keeps it under, once they
   * decide, or `null` when they declined.
   */
  async ask(ask: VaultAsk): Promise<string | null> {
    const args = Object.fromEntries(Object.entries(ask).filter(([, value]) => value !== undefined));
    const answer = await run(this.workspace, { path: "vault/vault", action: "ask" }, args);
    const reference = (answer.result as { reference?: unknown } | undefined)?.reference;
    return typeof reference === "string" ? reference : null;
  }

  /**
   * The reference of the vault item kept for the app and purpose `tags` names, or `null` when the
   * vault keeps none. It releases nothing, so the person is not asked.
   */
  async find(tags: VaultFind): Promise<string | null> {
    const daemon = daemonOf(this.workspace, "a vault item is found by");
    const answer = Daemon.answer(
      await (await daemon.opened()).find(await this.workspace.id(), tags.app, tags.purpose),
    );
    const reference = (answer as { reference?: unknown } | undefined)?.reference;
    return typeof reference === "string" ? reference : null;
  }
}

/**
 * The call lane of an app's own connection: how a bridge app carries one call of the outside
 * service it reaches into one of the workspace's calls. The app says which outside call it
 * carries into which workspace call (`carry`), who is in it (`report`), and hands over each Opus
 * frame its people say (`heard`); the device gives back what the workspace's people say
 * (`said`) and where it wants the app in the outside call (`asked`). Any other connection's
 * calls throw `Refused`.
 */
export class CallLane {
  /** The workspace whose call it carries. */
  readonly workspace: Workspace;
  #frames: CallFrame[] = [];
  #asked: CallAsk | null = null;

  constructor(workspace: Workspace) {
    this.workspace = workspace;
  }

  /** Carry the outside call `pair.outside` into the workspace call `pair.tilo`, or let go of the
   * one carried (`null`). */
  async carry(pair: CarriedCall | null): Promise<void> {
    const daemon = daemonOf(this.workspace, "a call is carried by");
    Daemon.answer(await (await daemon.opened()).carryCall(pair));
  }

  /** Say who is in the outside calls, and where the app stands in them. */
  async report(report: CallReport): Promise<void> {
    const daemon = daemonOf(this.workspace, "a call is carried by");
    Daemon.answer(await (await daemon.opened()).reportCall(report));
  }

  /** Hand the workspace's call one 20 ms Opus frame an outside person said. A frame said before
   * the connection is open is dropped, as live audio is. */
  async heard(frame: CallFrame): Promise<void> {
    const daemon = daemonOf(this.workspace, "a call is carried by");
    const ready = daemon.ready;
    if (ready === null) {
      void daemon.opened().catch(() => {});
      return;
    }
    const refused = ready.hearCall(frame.speaker, frame.opus);
    if (refused !== null) {
      Daemon.answer(refused);
    }
  }

  /** The frames the workspace's people said since the last time, oldest first. */
  async said(): Promise<CallFrame[]> {
    await this.#take();
    return this.#frames.splice(0, this.#frames.length);
  }

  /** Where the device last asked the app to be in the outside call, once: `null` when it asked
   * nothing new since. */
  async asked(): Promise<CallAsk | null> {
    await this.#take();
    const asked = this.#asked;
    this.#asked = null;
    return asked;
  }

  async #take(): Promise<void> {
    const daemon = daemonOf(this.workspace, "a call is carried by");
    const connection = await daemon.opened();
    const said = await answered(() => connection.saidCall());
    this.#frames.push(...said.frames.map((frame) => ({ ...frame, skipped: frame.skipped ?? 0 })));
    if (said.asked !== undefined && said.asked !== null) {
      this.#asked = { call: said.asked.call ?? null, status: said.asked.status ?? null };
    }
  }
}

/**
 * The presence lane of an app's own connection: the app's person says where they are and what
 * they do in its workspace (`say`), and the app reads who is composing a message in a resource
 * (`composing`), folded from the words the workspace's live session hands it exactly as a window
 * drawing that resource folds them for its "… is typing". The app's own words are never handed
 * back. Any other connection's calls throw `Refused`.
 */
export class PresenceLane {
  /** The workspace whose presence it says and reads. */
  readonly workspace: Workspace;

  constructor(workspace: Workspace) {
    this.workspace = workspace;
  }

  /** Say one word of the app's person: what they say under a presence key. */
  async say(said: PresenceSaid): Promise<void> {
    const daemon = daemonOf(this.workspace, "presence is said by");
    Daemon.answer(await (await daemon.opened()).sayPresence(said));
  }

  /**
   * Who is composing a message in `resource` now, whichever app they said it in: its id, or the
   * published address an action names it by (`tl:<app>:<id>`). Someone who stops, or goes quiet
   * for the system's linger, is not.
   */
  async composing(resource: string): Promise<PresenceMember[]> {
    const daemon = daemonOf(this.workspace, "presence is read by");
    const connection = await daemon.opened();
    return await answered(() => connection.composingAt(resource));
  }
}
