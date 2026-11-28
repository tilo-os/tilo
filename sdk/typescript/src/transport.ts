/**
 * How the client's requests reach Tilo's API: through this computer's daemon, which answers
 * `/v1` in process as whoever connected (the person, or an app the launcher started), or by URL
 * with a key. Either way a request is `(method, target, headers, body)` and its answer a web
 * `Response`; the generated client is written against that alone.
 *
 * The daemon's connection also carries what exists only there: the vault's lane (a web call
 * with a kept key, a release, a find) and an app's call lane.
 */

import { refusalOf } from "./errors.ts";

/** The SDK's version, as the hosted server reads it in the user agent. */
export const VERSION = "0.1.0";
/** The addon this computer's daemon is reached through. */
export const NATIVE_PACKAGE = "tilo-sdk-native";

/** One request to the API, `target` its path and query (`/v1/...`): its answer. */
export type Send = (
  method: string,
  target: string,
  headers: [string, string][],
  body: Uint8Array | null,
) => Promise<Response>;

/** `fetch`, unless you bring your own. */
export type FetchLike = typeof fetch;

/** A request with a key, by URL: `Authorization: Bearer <key>` at `url`. */
export function keySend(url: string, key: string, fetcher: FetchLike, signal: AbortSignal): Send {
  return (method, target, headers, body) =>
    fetcher(`${url}${target}`, {
      method,
      headers: [
        ...headers,
        ["authorization", `Bearer ${key}`],
        ["user-agent", `tilo-typescript/${VERSION}`],
      ],
      body: body as BodyInit | null,
      signal,
    });
}

/** An outside service's call and the workspace call it is carried into, as one call. */
export interface CarriedCall {
  /** The service's id of its call (a voice channel there). */
  outside: string;
  /** The workspace's voice channel it is one call with. */
  tilo: string;
}

/** One person in an outside call, as the service shows them. */
export interface CallPerson {
  /** Their account on the service. */
  account: string;
  /** The name the service shows for them. */
  name: string;
  /** Where the service serves their picture. */
  avatar_url?: string | null;
  /** BLAKE3 hex of their picture, once it is kept in the workspace's content. */
  avatar?: string | null;
  muted?: boolean;
  deafened?: boolean;
}

/** Where the app stands in the outside call it carries. */
export type CallLink =
  | { link: "idle" }
  | { link: "joining"; call: string }
  | { link: "joined"; call: string }
  | { link: "refused"; call: string; reason: string };

/** Who is in each outside call, and where the app stands in them. */
export interface CallReport {
  calls: { call: string; people: CallPerson[] }[];
  link: CallLink;
  /** The service's own line about its call; empty leaves it as it was. */
  status?: string;
}

/** One 20 ms Opus frame of a call (48 kHz), untouched, and the account that said it. */
export interface CallFrame {
  speaker: string;
  opus: Uint8Array;
  /** Frames of this stream the device dropped just before this one, so the receiver conceals
   * the gap. */
  skipped?: number;
}

/**
 * Where the device asks the app to be in the outside call: in `call`, showing `status` to its
 * people about who is on the workspace's side, or in none (`call: null`).
 */
export interface CallAsk {
  call: string | null;
  status: string | null;
}

/**
 * One presence word of an app's person: what they say (`payload`, such as
 * `{ composing: { composing: true } }`) under a presence key (`key`, such as
 * `{ resource: { app, id, kind } }`), in the shapes the workspace's live session carries.
 */
export interface PresenceSaid {
  key: Record<string, unknown>;
  payload: Record<string, unknown> | string;
}

/** A member of the workspace, as presence names them: `{ person }`, `{ agent: { id, person } }`. */
export type PresenceMember = Record<string, unknown>;

/** One answer of the daemon's connection: what it answered, or the refusal it was. */
type Answered = { answer?: unknown; refused?: unknown };

/** The daemon's connection, as the addon (`tilo-sdk-native`) hands it over. */
export interface DaemonConnection {
  /** The workspace an app was started in, or the one the daemon serves. */
  readonly workspace: string | null;
  /** One `/v1` request, answered in process as this connection's own identity. */
  request(
    method: string,
    target: string,
    headers: [string, string][],
    body: Buffer | null,
  ): Promise<{
    readonly status: number;
    readonly headers: [string, string][];
    next(): Promise<Buffer | null>;
  }>;
  webCall(workspace: string | null, args: Record<string, unknown>): Promise<Answered>;
  release(workspace: string | null, item: string): Promise<Answered>;
  find(workspace: string | null, app: string, purpose: string): Promise<Answered>;
  carryCall(pair: CarriedCall | null): Promise<Answered>;
  reportCall(report: CallReport): Promise<Answered>;
  hearCall(speaker: string, opus: Uint8Array): { refused: unknown } | null;
  saidCall(): {
    frames: { speaker: string; opus: Uint8Array; skipped?: number }[];
    asked?: { call?: string | null; status?: string | null } | null;
  };
  sayPresence(said: PresenceSaid): Promise<Answered>;
  /** Who composes in `resource` now; throws a refusal's body, as JSON, as its message. */
  composingAt(resource: string): PresenceMember[];
}

/** The addon's connection, made the first time a request needs it and again after one failed. */
export class Daemon {
  readonly #connect: () => Promise<DaemonConnection>;
  #opened: Promise<DaemonConnection> | null = null;
  /** The connection once made, for what a live call hands it without waiting. */
  ready: DaemonConnection | null = null;

  constructor(connect: () => Promise<DaemonConnection>) {
    this.#connect = connect;
  }

  /** This computer's daemon, through the addon: the connection the launcher handed an app it
   * started, else `socket`, else the daemon's own. */
  static native(socket: string | null, workspace: string | null): Daemon {
    return new Daemon(async () => {
      let addon: { connect(socket: string | null, workspace: string | null): Promise<DaemonConnection> };
      try {
        addon = await import(NATIVE_PACKAGE);
      } catch (error) {
        throw new Error(
          `this client reaches this computer's daemon through the ${NATIVE_PACKAGE} addon, which ` +
            `is not installed: install it with \`bun add ${NATIVE_PACKAGE}\`, or give the client a ` +
            `key (\`new Tilo({ key })\`) to reach Tilo by URL (${String(error)})`,
        );
      }
      return addon.connect(socket, workspace);
    });
  }

  /** The connection, made once. */
  opened(): Promise<DaemonConnection> {
    this.#opened ??= this.#connect().then(
      (connection) => (this.ready = connection),
      (error: unknown) => {
        this.#opened = null;
        throw error;
      },
    );
    return this.#opened;
  }

  /** One request through the daemon, its answer a `Response` whose body streams as it comes. */
  readonly send: Send = async (method, target, headers, body) => {
    const connection = await this.opened();
    const reply = await connection.request(
      method,
      target,
      headers,
      body === null ? null : Buffer.from(body.buffer, body.byteOffset, body.byteLength),
    );
    const bodiless = reply.status === 204 || reply.status === 205 || reply.status === 304;
    const stream = new ReadableStream<Uint8Array>({
      async pull(controller) {
        const piece = await reply.next();
        if (piece === null) {
          controller.close();
        } else {
          controller.enqueue(new Uint8Array(piece));
        }
      },
    });
    return new Response(bodiless ? null : stream, { status: reply.status, headers: reply.headers });
  };

  /** What one call of the daemon's own lanes answered: its answer, or its refusal thrown. */
  static answer(answered: Answered | null | undefined): unknown {
    if (answered?.refused !== undefined && answered.refused !== null) {
      throw refusalOf(answered.refused);
    }
    return answered?.answer;
  }
}
