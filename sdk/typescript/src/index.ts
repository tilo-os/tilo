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
  CallLane,
  Files,
  Members,
  Vault,
  WorkspaceKeys,
  type FileBytes,
  type VaultAsk,
  type VaultFind,
  type WebCallAnswer,
  type WebCallContent,
  type WebCallOptions,
  type WebCallPart,
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
  type CallAsk,
  type CallFrame,
  type CallLink,
  type CallPerson,
  type CallReport,
  type CallTaken,
  type CarriedCall,
  type FetchLike,
  type Group,
} from "./transport.ts";
export {
  AskPending,
  KeyExpired,
  KeyRefused,
  KeyRevoked,
  NOT_OFFERED,
  ProtocolError,
  Refused,
  SurfaceChanged,
  UNKNOWN_ACTION,
  Unreachable,
} from "./errors.ts";
export {
  type Action,
  type ActionAnswer,
  type ChangeEvent,
  type Event,
  type EventWhat,
  type Hit,
  type Invitation,
  type Key,
  Listing,
  type Member,
  type MintedKey,
  type OtherEvent,
  type Param,
  type Person,
  type ProgramAnswer,
  type Row,
  type StreamEvent,
  type InvocationInfo,
  InvocationList,
  type Usage,
  type WorkspaceInfo,
  type WorkspaceUsage,
  WorkspaceList,
  type WorkspaceExport,
} from "./models.ts";
