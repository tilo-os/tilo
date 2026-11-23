/**
 * Drive Tilo workspaces from JavaScript and TypeScript.
 *
 * ```ts
 * import { Tilo } from "tilo-sdk";
 *
 * const tilo = new Tilo();                    // key: TILO_API_KEY, API: TILO_API_URL
 * const ws = tilo.workspace("jane-doe");
 * const page = await ws.read("notes/Todo");
 * page.title = "Done";                        // a field edit
 * await page.flush();
 * ```
 *
 * Every call is one route of Tilo's `/v1` API (`docs/developers/api.md`).
 */

export {
  Keys,
  SignIn,
  Tilo,
  type TiloOptions,
  Workspaces,
  keyFrom,
  urlFrom,
} from "./client.ts";
export { Workspace, Files, Members, WorkspaceKeys, type FileBytes } from "./workspace.ts";
export { resourceProxy, kebab, made, segments } from "./resource.ts";
export type { Resource, Step, Surface, Entry } from "./resource.ts";
export { Invocation } from "./invocation.ts";
export { DEFAULT_URL, Endpoint, InvocationGroup, VERSION, withinInvocation, type Reply, type RequestOptions } from "./endpoint.ts";
export {
  AskPending,
  KeyExpired,
  KeyRefused,
  KeyRevoked,
  ProtocolError,
  Refused,
  SurfaceChanged,
  Unreachable,
  UnsupportedVersion,
} from "./errors.ts";
export {
  type Action,
  type ActionAnswer,
  type ApiInfo,
  type Event,
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
  type Upload,
  type Usage,
  type WorkspaceInfo,
  type WorkspaceUsage,
  WorkspaceList,
  type WorkspaceExport,
} from "./models.ts";
export {
  HttpTransport,
  NativeTransport,
  HeaderMap,
  NATIVE_PACKAGE,
  type FetchLike,
  type HeadersLike,
  type HostOptions,
  type Transport,
  type TransportReply,
  type TransportRequest,
} from "./transport.ts";
export { API_VERSION, encodePath, routes, splitAction } from "./routes.ts";
