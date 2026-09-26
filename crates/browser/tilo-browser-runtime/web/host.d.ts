import type {DesktopNavigationV1} from "./navigation";
export type * from "./navigation";
import type {CertifiedActionsV1,PrivateInvocation} from './peer';
/**
 * What a mount runs: the id of any application compiled into the build
 * (`so.tilo.word-processor`, `so.tilo.ebook`, …; see `applications()`), the
 * Cloud frontend's aliases `writer` and `chat`, or `desktop` for the canonical
 * desktop, which requires an admitted certified peer. An id the build does not
 * contain rejects the mount with `application "…" is not available`.
 */
export type Application = 'writer' | 'chat' | 'desktop' | (string & {});
export interface ApplicationInfo {
  /** The full app id an alias resolves to. */
  id: string;
  title: string;
  /**
   * `ordinary`: a mount opens `record`, else reopens this room's last record of
   * the app, else (or with `newDocument`) creates one. `package-primary`: every
   * mount opens the room's one document (Chat's workspace).
   */
  scope: 'ordinary' | 'package-primary';
}
export interface RuntimeStatus {
  phase?: 'gpu-pending' | 'presented' | 'failed' | 'closed';
  error?: string;
  persistence?: 'committed' | 'failed';
  frames?: number;
  /** Private caller observation; never a claim of replicated document completion. */
  action?: Pick<PrivateInvocation,'invocation'|'state'|'terminal'|'cancellationRequested'>;
  actionError?: string;
  /** Native semantic command receipt lifecycle; signed room state owns rendering. */
  desktopCommand?: { invocation: string; state: string; terminal: boolean; error: string | null };
  desktopErrors?: string[];
  document?: unknown;
  heads?: string[];
  [diagnostic: string]: unknown;
}
export interface MountOptions {
  onNavigation?: (controller: DesktopNavigationV1) => void;
  app?: Application;
  actionsV1?: CertifiedActionsV1;
  profile?: string;
  device?: string;
  room?: string;
  author?: string;
  instance?: number;
  record?: string;
  newDocument?: boolean;
  mac?: boolean;
  signal?: AbortSignal;
  onStatus?: (status: RuntimeStatus) => void;
  loadAsset?: (url: string, signal: AbortSignal) => Promise<Uint8Array>;
}
/** One published action, as `tilo-control actions list` describes it. */
export interface ActionDescriptor {
  address: {resource: string; action: string};
  resource_kind: string;
  /** The resource's label as the app words it (a note's text, a highlight's passage). */
  resource_label: string;
  action_label: string;
  description?: string | null;
  kind: 'Document' | 'View' | 'Effect' | string;
  /** Typed arguments `invokeAction` binds strictly; a prompted action takes `{value}`. */
  params: {name: string; ty: string; required: boolean; description: string; choices?: string[]; schema?: unknown}[];
  contract: {confirmation?: unknown; risk?: string; [field: string]: unknown};
  [field: string]: unknown;
}
export interface ActionResult {
  /** `accepted`: a background job continues (a picker, an import). */
  state: 'completed' | 'accepted';
  output?: {value?: unknown; resources?: string[]};
}
export interface DesktopMount {
  navigation?: DesktopNavigationV1;
  /** The mounted application; absent for the canonical desktop. */
  readonly application?: ApplicationInfo;
  /**
   * The record this mount opened or created, once it has presented. Another
   * mount in the page with the same profile, device, room and app that passes
   * it as `record` edits the same document and shows the other's changes live.
   * Absent for a package-primary app and for the canonical desktop.
   */
  readonly record?: string;
  /**
   * The actions the mounted application publishes now, with its permissions
   * applied. `resource` narrows to one resource and its children, including
   * one the application resolves only on request. Throws for the canonical desktop.
   */
  actions(resource?: string): ActionDescriptor[];
  /**
   * Invoke one published action through the application's action door, the
   * one the palette, agents and assistive technology use. Throws
   * `code: reason` when the address is not published now, the permissions
   * refuse it, the arguments do not bind, or its contract requires
   * confirmation and `confirm` is not set.
   */
  invokeAction(resource: string, action: string, args?: Record<string, unknown>, options?: {confirm?: boolean}): ActionResult;
  cleanup(): Promise<void>;
  flush(): Promise<void>;
  state(): RuntimeStatus | undefined;
  readonly canvas: HTMLCanvasElement;
  readonly editor: HTMLTextAreaElement;
  readonly semantics: HTMLElement;
  semantic(path: string, target: string | undefined, action: string, value: string, anchor: number, focus: number): unknown;
  durableState(): Promise<unknown[]>;
  failNextWrite(): void;
  destroyDevice(): unknown;
  recover(): unknown;
  retryAsset(assetId:string):Promise<void>;
  probePixels(): Promise<unknown>;
}
/** Resolves after the first actual GPU presentation. AbortSignal cancels pending initialization. */
export function mountDesktop(target: HTMLElement, options?: MountOptions): Promise<DesktopMount>;
/** Every application this build can mount standalone. */
export function applications(): Promise<ApplicationInfo[]>;
/** Terminal page-host shutdown. Individual peers must use their own cleanup method. */
export function shutdown(): Promise<void>;

/** Retry retained close failures before releasing exclusive actor custody. */
export function retryPendingRecovery(): Promise<void>;

/** Number of failed closes retaining actor custody and pending durable bytes. */
export function pendingRecoveryCount(): number;

/** Actual retained Rust component reports, including captured view envelopes. */
export function pendingComponentCloseCount(): number;
