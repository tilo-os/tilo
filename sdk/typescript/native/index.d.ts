/** A key file's contents: the host it reaches, its name, its key and certificate (hex). */
export interface KeyFile {
  host: string;
  name: string;
  key: string;
  cert: string;
}

/** An answer of the API: the body whole, or a `text/event-stream` answer's chunks. */
export interface Reply {
  status: number;
  /** A name's values joined with `, `. */
  headers: Record<string, string>;
  body: Uint8Array | AsyncIterable<Uint8Array>;
}

/** A Tilo host reached directly with the key a key file holds. */
export declare class Host {
  /** `keyFile`: the path of a key file, or its contents. */
  constructor(keyFile: string | KeyFile | object);
  /** Answer `method` on `path` (with its query) as the API does. */
  request(
    method: string,
    path: string,
    headers: Record<string, string>,
    body: Uint8Array,
  ): Promise<Reply>;
}

/** How tilo-sdk's `Tilo.host()` opens a host: a key file, or a key and its certificate. */
export interface OpenOptions {
  /** The host's origin; with a key file, it takes the place of the file's. */
  url?: string;
  keyFile?: string;
  /** The key's 32-byte secret and its certificate, hex or bytes. */
  key?: string | Uint8Array;
  cert?: string | Uint8Array;
  /** What the host's people see the key as, when it comes without a key file. */
  name?: string;
}

/** The host `options` name, opened. */
export declare function open(options: OpenOptions): Promise<Host>;
