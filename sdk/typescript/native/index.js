// tilo-sdk-native: the binary built for this platform (src/lib.rs), and a streamed answer's
// chunks as an async iterable.
//
// Each platform's binary is its own package (npm/<platform>/), an optional dependency of this
// one, so an install fetches only the one it runs on, with the worker program it starts. A binary beside this file wins: that is a
// build of the tree (tools/ci/sdk-native.sh). Loading waits for the first Host, so importing
// the package never fails; constructing a Host on a platform with no binary says which.
import { createRequire } from "node:module";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);

// The platforms a binary is published for, as `${process.platform}-${process.arch}`, and the
// name of each one's binary and package.
const PLATFORMS = {
  "linux-x64": "linux-x64-gnu",
};

let native;

function load() {
  if (native) return native;
  const platform = PLATFORMS[`${process.platform}-${process.arch}`];
  if (!platform) {
    throw new Error(
      `tilo-sdk-native has no binary for ${process.platform}-${process.arch} ` +
        `(it has: ${Object.values(PLATFORMS).join(", ")}); reach Tilo through its API server instead`,
    );
  }
  const local = fileURLToPath(new URL(`tilo-sdk-native.${platform}.node`, import.meta.url));
  const binary = existsSync(local) ? local : require.resolve(`tilo-sdk-native-${platform}`);
  // The Automerge bytes a host sends are decoded in sandboxed workers, which start as the
  // worker program beside the binary: Node cannot be started again as one.
  const worker = join(dirname(binary), "tilo-automerge-worker");
  if (!existsSync(worker)) throw new Error(`tilo-sdk-native is incomplete: ${worker} is missing`);
  const loaded = require(binary);
  loaded.serveWorkersFrom(worker);
  native = loaded;
  return native;
}

async function* chunksOf(chunks) {
  for (;;) {
    const chunk = await chunks.next();
    if (chunk == null) return;
    yield chunk;
  }
}

export class Host {
  #host;

  constructor(keyFile) {
    const { Host: NativeHost } = load();
    this.#host = new NativeHost(keyFile);
  }

  async request(method, path, headers, body) {
    const reply = await this.#host.request(method, path, headers, body);
    const whole = reply.body instanceof Uint8Array;
    return {
      status: reply.status,
      headers: reply.headers,
      body: whole ? reply.body : chunksOf(reply.body),
    };
  }
}

function hex(value) {
  return typeof value === "string" ? value : Buffer.from(value).toString("hex");
}

/**
 * The host at `url`, as tilo-sdk's `Tilo.host()` opens it: with the key file `keyFile` names, or
 * with `key` and `cert` given as they are, hex or bytes.
 */
export async function open({ url, keyFile, key, cert, name = "tilo-sdk" }) {
  if (keyFile !== undefined) {
    const contents = JSON.parse(readFileSync(keyFile, "utf8"));
    return new Host(url === undefined ? contents : { ...contents, host: url });
  }
  if (key === undefined || cert === undefined) {
    throw new Error("a host is opened with a key file, or with a key and its certificate");
  }
  return new Host({ host: url, name, key: hex(key), cert: hex(cert) });
}
