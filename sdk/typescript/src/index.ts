/**
 * Drive Tilo workspaces from JavaScript and TypeScript.
 *
 * ```ts
 * import { Tilo } from "@tilo/sdk";
 *
 * const tilo = new Tilo();                    // key: TILO_API_KEY, door: TILO_DOOR
 * const ws = tilo.workspace("jane-doe");
 * const page = await ws.read("notes/Todo");
 * page.title = "Done";                        // a field edit
 * await page.flush();
 * ```
 */

export { Tilo, Workspaces, Keys, SignIn, type TiloOptions } from "./client.ts";
export { Workspace, Files, Members, WorkspaceKeys } from "./workspace.ts";
export { resourceProxy, kebab, segments } from "./resource.ts";
export type { Resource, Step } from "./resource.ts";
export { Run } from "./run.ts";
export { RunGroup, Link, withinRun } from "./link.ts";
export { Refused, RateLimited, AskPending, ProtocolError } from "./errors.ts";
export {
  type Event,
  type Hit,
  type Host,
  type Invitation,
  type Key,
  Listing,
  type Member,
  type MintedKey,
  type Person,
  type ProgramAnswer,
  type Row,
  type WorkspaceInfo,
  WorkspaceList,
} from "./models.ts";
export {
  DEFAULT_DOOR,
  SESSION_HEADER,
  SURFACE,
  Transport,
  doorFrom,
  keyFrom,
  type Answer,
  type FetchLike,
} from "./wire.ts";

export const VERSION = "0.1.0";
