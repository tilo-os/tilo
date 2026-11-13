/**
 * Where a request goes: the one seam under the API.
 *
 * The SDK speaks Tilo's `/v1` API (`docs/developers/api.md`), and two things
 * serve it: an API server, reached with `fetch` ([`HttpTransport`]), and a
 * Tilo host in process, through the native addon ([`NativeTransport`],
 * `Tilo.host(…)`). Both take a method, a path, headers and a body and answer
 * the status, the headers and the body's bytes; the routes and the JSON above
 * them are the same either way.
 */

import { NetworkError, ProtocolError } from "./errors.ts";

/** The one function a client needs to reach an API server: `fetch`, unless you bring your own. */
export type FetchLike = typeof fetch;

/** What a request carries, beside its method and path. */
export interface TransportRequest {
  /** A JSON body: sent as `application/json`. */
  json?: unknown;
  /** Bytes, as they are: an upload's file, or nothing. */
  data?: Uint8Array | ArrayBuffer | string;
  headers?: Record<string, string>;
  /** The body is wanted as it arrives: a stream of events, or a file's bytes. */
  stream?: boolean;
  signal?: AbortSignal;
}

/** What a reply's headers answer one name with: `fetch`'s `Headers`, or a map. */
export interface HeadersLike {
  get(name: string): string | null;
}

/** A request's answer: its status, its headers, and its body as bytes or as they arrive. */
export interface TransportReply {
  status: number;
  headers: HeadersLike;
  body: Uint8Array | AsyncIterable<Uint8Array> | null;
}

/** One way to reach `/v1`: over HTTP, or in process. */
export interface Transport {
  request(
    method: string,
    path: string,
    options?: TransportRequest,
  ): Promise<TransportReply>;
}

/** The body of a request, as bytes: JSON, or what a caller gave. */
export function bytesOf(body: unknown): Uint8Array {
  if (body instanceof Uint8Array) {
    return body;
  }
  if (body instanceof ArrayBuffer) {
    return new Uint8Array(body);
  }
  if (typeof body === "string") {
    return new TextEncoder().encode(body);
  }
  return new TextEncoder().encode(JSON.stringify(body ?? null));
}

/** The bytes of a reply's body, buffered whole. */
export async function collect(body: AsyncIterable<Uint8Array> | Uint8Array | null): Promise<Uint8Array> {
  if (body === null) {
    return new Uint8Array();
  }
  if (body instanceof Uint8Array) {
    return body;
  }
  const pieces: Uint8Array[] = [];
  let total = 0;
  for await (const piece of body) {
    pieces.push(piece);
    total += piece.length;
  }
  const whole = new Uint8Array(total);
  let at = 0;
  for (const piece of pieces) {
    whole.set(piece, at);
    at += piece.length;
  }
  return whole;
}

/** A reply's body as a stream of bytes, however the transport gave it. */
export async function* chunksOf(
  body: AsyncIterable<Uint8Array> | Uint8Array | null,
): AsyncGenerator<Uint8Array> {
  if (body === null) {
    return;
  }
  if (body instanceof Uint8Array) {
    if (body.length > 0) {
      yield body;
    }
    return;
  }
  yield* body;
}

/** A map of headers, answered case-insensitively as `fetch`'s `Headers` does. */
export class HeaderMap implements HeadersLike {
  readonly #fields = new Map<string, string>();

  constructor(fields: Record<string, string> = {}) {
    for (const [name, value] of Object.entries(fields)) {
      this.#fields.set(name.toLowerCase(), String(value));
    }
  }

  /** The value of `name`, whatever its case. */
  get(name: string): string | null {
    return this.#fields.get(name.toLowerCase()) ?? null;
  }
}

/**
 * `/v1` over HTTP, with `fetch`: Node 20, Bun and browsers all serve it.
 *
 * The key goes with every call it makes, as `Authorization: Bearer`; the
 * device flow for a new key (`/keys/new`, `/keys/poll`) goes through the same
 * transport without one.
 */
export class HttpTransport implements Transport {
  readonly url: string;
  readonly key: string | null;
  readonly fetch: FetchLike;

  /** `url` is the API server's origin, without a trailing slash. */
  constructor(url: string, key: string | null = null, fetcher: FetchLike = globalThis.fetch) {
    this.url = url.replace(/\/+$/, "");
    this.key = key;
    this.fetch = fetcher;
  }

  /** One request, its answer: its status, its headers and its body. */
  async request(
    method: string,
    path: string,
    options: TransportRequest = {},
  ): Promise<TransportReply> {
    const headers: Record<string, string> = { ...options.headers };
    if (this.key !== null) {
      headers.authorization = `Bearer ${this.key}`;
    }
    let body: BodyInit | undefined;
    if (options.json !== undefined) {
      headers["content-type"] = "application/json";
      body = JSON.stringify(options.json);
    } else if (options.data !== undefined) {
      body = options.data as BodyInit;
    }
    const url = new URL(path.startsWith("/") ? path : `/${path}`, this.url).toString();
    let response: Response;
    try {
      response = await this.fetch(url, { method, headers, body, signal: options.signal });
    } catch (error) {
      throw new NetworkError(`could not reach the API server at ${this.url}: ${String(error)}`);
    }
    return {
      status: response.status,
      headers: response.headers,
      body: options.stream && response.body !== null ? bytesOfStream(response.body) : await bytes(response),
    };
  }
}

async function bytes(response: Response): Promise<Uint8Array> {
  try {
    return new Uint8Array(await response.arrayBuffer());
  } catch (error) {
    throw new NetworkError(`the API server's answer was cut short: ${String(error)}`);
  }
}

async function* bytesOfStream(stream: ReadableStream<Uint8Array>): AsyncGenerator<Uint8Array> {
  const reader = stream.getReader();
  try {
    for (;;) {
      let read: { done: boolean; value?: Uint8Array };
      try {
        read = await reader.read();
      } catch (error) {
        throw new NetworkError(`the API server's stream was cut short: ${String(error)}`);
      }
      if (read.done || read.value === undefined) {
        return;
      }
      yield read.value;
    }
  } finally {
    reader.releaseLock();
  }
}

/** The addon an in-process host is reached through, loaded when one is opened. */
export const NATIVE_PACKAGE = "tilo-sdk-native";

/** How a host is opened: the file its key lives in, or the key and its certificate. */
export interface HostOptions {
  /** The device key file the host's Tilo keeps, as `TILO_KEY_FILE` names it. */
  keyFile?: string;
  /** The key itself, and its certificate, when they are not in a file. */
  key?: string | Uint8Array;
  cert?: string | Uint8Array;
}

/** What the addon answers: the host, as the routes' transport. */
interface NativeHost {
  request(
    method: string,
    path: string,
    headers: Record<string, string>,
    body: Uint8Array,
  ): Promise<{
    status: number;
    headers: Record<string, string>;
    body: Uint8Array | AsyncIterable<Uint8Array>;
  }>;
}

/**
 * `/v1` served in process by a Tilo host: the same routes the API server
 * answers, without a socket, from the native addon.
 */
export class NativeTransport implements Transport {
  readonly host: NativeHost;

  constructor(host: NativeHost) {
    this.host = host;
  }

  /** Open the host at `origin`: its key from a file, or the key and certificate given. */
  static async open(origin: string, options: HostOptions = {}): Promise<NativeTransport> {
    const module = await nativeAddon();
    const open = module.open ?? module.default;
    if (typeof open !== "function") {
      throw new ProtocolError(
        `${NATIVE_PACKAGE} answers no open(): install it again, from the Tilo tree`,
      );
    }
    const opened = (await open({ url: origin, ...options })) as Partial<NativeHost> | null;
    if (!opened || typeof opened.request !== "function") {
      throw new ProtocolError(`${NATIVE_PACKAGE} opened no host to request through`);
    }
    return new NativeTransport(opened as NativeHost);
  }

  /** One request to the host, its answer as the addon gives it. */
  async request(
    method: string,
    path: string,
    options: TransportRequest = {},
  ): Promise<TransportReply> {
    const headers: Record<string, string> = { ...options.headers };
    let data: Uint8Array = new Uint8Array();
    if (options.json !== undefined) {
      headers["content-type"] = "application/json";
      data = bytesOf(options.json);
    } else if (options.data !== undefined) {
      data = bytesOf(options.data);
    }
    const reply = await this.host.request(method, path, headers, data);
    return {
      status: reply.status,
      headers: new HeaderMap(reply.headers),
      body: options.stream ? reply.body : await collect(reply.body),
    };
  }
}

interface NativeModule {
  open?: (options: { url: string } & HostOptions) => Promise<unknown>;
  default?: ((options: { url: string } & HostOptions) => Promise<unknown>) | undefined;
}

/** The addon, loaded the first time a host is opened; named when it is missing. */
async function nativeAddon(): Promise<NativeModule> {
  try {
    return (await import(NATIVE_PACKAGE)) as NativeModule;
  } catch (error) {
    throw new ProtocolError(
      `an in-process Tilo host needs the ${NATIVE_PACKAGE} addon, which is not installed: ` +
        `install it with \`bun add ${NATIVE_PACKAGE}\`, or point a client at an API server ` +
        `instead (\`new Tilo({ url })\`); the addon was built from the Tilo tree (${String(error)})`,
    );
  }
}
