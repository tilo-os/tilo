// tilo-sdk-native: this computer's daemon, reached over its native endpoint as a connection
// admitted to whoever makes it (the person, or an app process the launcher started).
//
// Each platform's binary is its own package (npm/<platform>/), an optional dependency of this
// one, so an install fetches only the one it runs on, with the worker program it starts. A
// binary beside this file wins: that is a build of the tree (tools/ci/sdk-native.sh). Loading
// waits for the first connection, so importing the package never fails; connecting on a
// platform with no binary says which.
import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);

// The platforms a binary is published for, as `${process.platform}-${process.arch}`, and the
// name of each one's binary and package.
const PLATFORMS = {
  "linux-x64": "linux-x64-gnu",
  "linux-arm64": "linux-arm64-gnu",
};

let native;

function load() {
  if (native) return native;
  const platform = PLATFORMS[`${process.platform}-${process.arch}`];
  if (!platform) {
    throw new Error(
      `tilo-sdk-native has no binary for ${process.platform}-${process.arch} ` +
        `(it has: ${Object.values(PLATFORMS).join(", ")}); give the client a key instead`,
    );
  }
  const local = fileURLToPath(new URL(`tilo-sdk-native.${platform}.node`, import.meta.url));
  const binary = existsSync(local) ? local : require.resolve(`tilo-sdk-native-${platform}`);
  // The Automerge bytes a call reads are decoded in sandboxed workers, which start as the
  // worker program beside the binary: Node cannot be started again as one.
  const worker = join(dirname(binary), "tilo-automerge-worker");
  if (!existsSync(worker)) throw new Error(`tilo-sdk-native is incomplete: ${worker} is missing`);
  const loaded = require(binary);
  loaded.serveWorkersFrom(worker);
  native = loaded;
  return native;
}

/**
 * This computer's daemon, connected: the connection the launcher handed an app it started,
 * else `socket`, else the daemon this computer runs. `workspace` names the one workspace the
 * connection acts in when it knows it; both are usually `null`.
 */
export async function connect(socket = null, workspace = null) {
  return load().connect(socket, workspace);
}
