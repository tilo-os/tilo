/**
 * Drive Tilo workspaces from JavaScript and TypeScript.
 *
 * ```ts
 * import { Tilo } from "tilo-sdk";
 *
 * const tilo = new Tilo();                    // this computer's daemon
 * const ws = tilo.workspace();                // the workspace the daemon serves
 * const page = await ws.read("notes/Todo");
 * page.title = "Done";                        // a field edit
 * await page.flush();
 * ```
 *
 * Every call is one tool of Tilo's agent surface (`tilo_ls`, `tilo_read`,
 * `tilo_do`, …), carried by this computer's daemon through the native addon,
 * or by a key to the hosted server (`new Tilo({ key })`).
 */

export {
  DEFAULT_URL,
  RETRY_DEADLINE_MS,
  Keys,
  Tilo,
  type TiloOptions,
  Workspaces,
  keyFrom,
  urlFrom,
} from "./client.ts";
export {
  Workspace,
  Files,
  Members,
  Vault,
  WorkspaceKeys,
  type FileBytes,
  type VaultAsk,
  type WebCallAnswer,
  type WebCallOptions,
} from "./workspace.ts";
export { resourceProxy, kebab, made, segments } from "./resource.ts";
export type { Resource, Step, Surface, Entry } from "./resource.ts";
export { Invocation } from "./invocation.ts";
export { Endpoint, InvocationGroup, withinInvocation } from "./endpoint.ts";
export {
  DaemonTransport,
  KeyTransport,
  NATIVE_PACKAGE,
  VERSION,
  type Transport,
  type FetchLike,
  type Group,
} from "./transport.ts";
export {
  AskPending,
  KeyExpired,
  KeyRefused,
  KeyRevoked,
  ProtocolError,
  Refused,
  SurfaceChanged,
  Unreachable,
} from "./errors.ts";
export {
  type Action,
  type ActionAnswer,
  type Event,
  type EventWhat,
  type Hit,
  type Invitation,
  type Key,
  Listing,
  type Member,
  type MintedKey,
  type Param,
  type Person,
  type ProgramAnswer,
  type Row,
  type InvocationInfo,
  InvocationList,
  type Usage,
  type WorkspaceInfo,
  type WorkspaceUsage,
  WorkspaceList,
  type WorkspaceExport,
} from "./models.ts";
