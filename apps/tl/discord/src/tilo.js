// The workspace as the bridge's own process sees it: calls to actions, reads, watches and the
// vault. The SDK answers `Tilo()` from the daemon's frames with `TILO_SVC_NATIVE_FD` and
// `TILO_WORKSPACE_ID`; a test hands the same shape in memory.
//
// A secret never passes through here as a value except `release`, which the gateway's login
// needs in this process: every other request names a vault item and the request is answered
// with the secret added server-side.

import { SELF } from "./plan.js";

export class Calls {
  /// `ws`: the SDK workspace (`ws.do`, `ws.read`, `ws.watch`, `ws.vault.*`).
  constructor(ws) {
    this.ws = ws;
    this.calls = [];
    this.made = [];
  }

  /// Run `call` (`{app, action, args, map?}`), keeping what it made where `map` says. The
  /// Discord id is kept against the resource the call answered.
  async do(call) {
    const address = `${call.app}#${call.action}`;
    this.calls.push({ address, args: call.args });
    const result = await this.ws.do(address, call.args);
    const made = referenceId(result);
    if (call.map && made) {
      const tilo = call.app === "so.tilo.chat" ? publishedOf(made) : made;
      await this.ws.do(`${SELF}#map`, {
        kind: call.map.kind,
        tilo,
        discord: call.map.discord,
        origin: call.map.origin ?? "discord",
        persona: call.map.persona ?? "",
        root: call.map.root ?? "",
      });
      this.made.push({ ...call.map, tilo });
    }
    return made;
  }

  /// The bridge's record as the workspace holds it.
  async state(path) {
    return this.ws.read(path);
  }

  /// What the vault holds for `item`, released to this process for the gateway's login.
  async release(item) {
    return this.ws.vault.release(item);
  }

  /// Ask the person for a secret: they type it into the system's own protected field on a device
  /// of theirs, and the vault keeps it for the place asked for, answering where. `null` when
  /// they declined.
  async ask(question) {
    return this.ws.vault.ask(question);
  }

  /// One request with a vault item added server-side: REST calls never hold the secret.
  async webCall(request) {
    return this.ws.vault.webCall(request);
  }

  /// Changes as the workspace reports them.
  async *watch(path) {
    yield* this.ws.watch(path);
  }

  /// This app's call lane, when the workspace client has one: the daemon's side of a call
  /// carried from an outside service. `null` for a client that carries none, which is a bridge
  /// that reads and writes but never joins a call.
  lane() {
    const lane = this.ws.callLane;
    if (!lane) return null;
    return {
      carry: (pair) => lane.carry(pair),
      report: (report) => lane.report(report),
      heard: (frame) => lane.heard(frame),
      said: () => lane.said(),
    };
  }
}

/// The id a call answered: a resource reference's id, or the id text itself.
export function referenceId(result) {
  if (!result) return null;
  if (typeof result === "string") return result;
  if (typeof result.id === "string") return result.id;
  if (typeof result.ref === "string") return result.ref;
  if (result.item && typeof result.item.id === "string") return result.item.id;
  if (typeof result.value === "string") return result.value;
  if (result.value && typeof result.value.id === "string") return result.value.id;
  return null;
}

/// A resource's published id, from the reference a call answered.
export function publishedOf(id) {
  const text = String(id ?? "");
  return text.startsWith("tl:") ? text : `tl:so.tilo.chat:${text}`;
}
