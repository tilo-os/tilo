// The workspace client this app's own process speaks through: the SDK's client of the daemon's
// native connection. The launcher connects it before the sandbox starts and passes it as
// `TILO_SVC_NATIVE_FD`; the daemon admits every call on it as this app in `TILO_WORKSPACE_ID`.
//
// Its shape is what `src/tilo.js` uses: `do(address, args)`, `read(path)`, `watch(path)` (each
// message of a bridged conversation and what happened to it), and `vault.release(item)` and
// `vault.webCall(request)`. A build that carries no such client refuses here, in words a person
// reading the device's log can act on, rather than running a bridge that carries nothing.

/// The workspace client of the process at `TILO_SVC_NATIVE_FD`.
export function Tilo() {
  if (!process.env.TILO_SVC_NATIVE_FD) {
    throw new Error(
      "this process was not started as the app's connection: the launcher hands it " +
        "TILO_SVC_NATIVE_FD, and nothing else may act as the app",
    );
  }
  throw new Error(
    "the SDK's native-frames client is not in this build: a bridge needs do, read, watch, " +
      "vault.release and vault.webCall on TILO_SVC_NATIVE_FD before it can carry anything",
  );
}
