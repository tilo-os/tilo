/**
 * Drive Tilo workspaces from JavaScript and TypeScript, through Tilo's API, `/v1`.
 *
 * ```ts
 * import { Tilo, run } from "tilo-sdk";
 *
 * const tilo = new Tilo();                     // this computer's daemon
 * const ws = tilo.workspace();                 // the workspace it serves
 * const page = await ws.resource("notes/Todo").load();
 * page.title = "Done";                         // a field edit
 * await page.flush();
 * await run(ws, { path: "notes", action: "new-note" }, { title: "Next" });
 * ```
 *
 * `Tilo` and `Workspace` are the API's operations, written from its description by
 * `tilo sdk gen`: `ws.listResources`, `ws.runAction`, `ws.events`, `ws.putContent`, … Each
 * goes through this computer's daemon (the `tilo-sdk-native` addon), which answers `/v1` as
 * whoever connected, or by URL with a key (`new Tilo({ key })`).
 */

export * from "./types/api-v1.ts";
export {
  DEFAULT_URL,
  InvocationGroup,
  RETRY_DEADLINE_MS,
  Tilo,
  type TiloOptions,
  Workspace,
  keyFrom,
  run,
  urlFrom,
} from "./client.ts";
export { Refused } from "./errors.ts";
export {
  exported,
  type Exported,
  kebab,
  made,
  resourceProxy,
  rows,
  segments,
  type Entry,
  type Resource,
  type Step,
  type Surface,
} from "./resource.ts";
export {
  Daemon,
  NATIVE_PACKAGE,
  VERSION,
  type CallAsk,
  type CallFrame,
  type CallLink,
  type CallPerson,
  type CallReport,
  type CarriedCall,
  type DaemonConnection,
  type FetchLike,
  type Send,
} from "./transport.ts";
export {
  CallLane,
  Vault,
  type VaultAsk,
  type VaultFind,
  type WebCallAnswer,
  type WebCallContent,
  type WebCallOptions,
  type WebCallPart,
} from "./vault.ts";
