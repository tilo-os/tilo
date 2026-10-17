import type {CaptureSource} from './panorama';
export {createPanorama} from './panorama';
export type {CaptureSource, PanoramaCamera, CaptureBounds, PanoramaReceipt, PanoramaObserver} from './panorama';
import type {DesktopNavigationV1} from "./navigation";
export type * from "./navigation";
import type {CertifiedActionsV1,PrivateInvocation} from './peer';
/**
 * What a mount runs: the id of any application compiled into the runtime
 * (`so.tilo.word-processor`, `so.tilo.ebook`, …), or `desktop` for the
 * canonical desktop. A page URL selects it through the link grammar in
 * `docs/architecture/web-routing.md` (the `app/` entry is a single-app mount).
 */
export type Application = 'desktop' | (string & {});
export interface RuntimeStatus {
  /**
   * `crashed`: an application panicked and the runtime stopped. With
   * `crashedWindow` (its tab id) the window is kept out of the room's next
   * load, which shows `error` in it; `restarting` says the page reloads.
   */
  phase?: 'gpu-pending' | 'presented' | 'failed' | 'closed' | 'crashed';
  crashedWindow?: string | null;
  restarting?: boolean;
  app?: string;
  error?: string;
  persistence?: 'committed' | 'failed';
  frames?: number;
  /** Exact instance used with publication aliases; absent on desktop mounts. */
  componentInstance?: number;
  presentation?: boolean;
  layoutGeneration?: number;
  /** Private caller observation; never a claim of replicated document completion. */
  action?: Pick<PrivateInvocation,'invocation'|'state'|'terminal'|'cancellationRequested'>;
  actionError?: string;
  /** Native semantic command receipt lifecycle; signed room state owns rendering. */
  desktopCommand?: { invocation: string; state: string; terminal: boolean; error: string | null };
  desktopErrors?: string[];
  /** A desktop mount's active workspace, as its top bar names it. */
  workspaceName?: string | null;
  document?: unknown;
  heads?: string[];
  [diagnostic: string]: unknown;
}
export interface MountOptions {
  /** Host-issued session for tilo-model-http; never a provider API key.
   * Bound only to this mount. No compute node or certified peer is required.
   * The service authenticates the session and authorizes the selected profile.
   */
  models?: {
    /** Absolute HTTPS URL ending in /model/rpc. */
    endpoint: string;
    sessionToken: string;
    /** Supply both selection fields, or omit both for the backend's user default. */
    profileId?: string;
    modelId?: string;
  };
  /** Explicit host-session grant for Model Providers or native Settings only; the backend still authorizes
   * every operation. Never falls back to models.sessionToken. No provider keys.
   */
  providerManagement?: {
    endpoint: string;
    sessionToken: string;
  };
  onNavigation?: (controller: DesktopNavigationV1) => void;
  app?: Application;
  actionsV1?: CertifiedActionsV1;
  profile?: string;
  device?: string;
  room?: string;
  /** Who the mount's edits and actions are attributed to. */
  author?: string;
  instance?: number;
  record?: string;
  newDocument?: boolean;
  mac?: boolean;
  signal?: AbortSignal;
  onStatus?: (status: RuntimeStatus) => void;
  /**
   * Reload the page after an application crashed the runtime (default
   * true), which starts the desktop again without that window; `false`
   * leaves it to the caller after the `crashed` status.
   */
  restartOnCrash?: boolean;
  loadAsset?: (url: string, signal: AbortSignal) => Promise<Uint8Array>;
  /**
   * The desktop's Invite (its launchpad card and the people control in its
   * top bar) calls this; without it the desktop offers no invitation.
   */
  onInvite?: () => void;
  /**
   * What only the page can do for the person (keeping their identity):
   * desktop search lists these beside its own commands, and choosing one,
   * there or on a banner `offerHostAction` showed, calls `onHostAction(id)`
   * inside the person's gesture.
   */
  hostActions?: HostAction[];
  onHostAction?: (id: string) => void;
  /** The workspace name a desktop this mount seeds starts with. */
  workspaceName?: string;
  /**
   * The host of the room shown (`example.org`), which the top bar names in
   * a quiet pill beside the workspace; leave it out on the person's own
   * desktop.
   */
  host?: string;
  /**
   * The links the desktop's About sheet (behind the Tilo mark in its top
   * bar) offers; an empty or missing one is not offered.
   */
  about?: {notify?: string; source?: string; community?: string};
}
/** Which window of a desktop an action call addresses. */
export interface WindowOption {
  /** The window's tab id (a navigation route's `tab`); defaults to the focused window. */
  window?: string;
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
/** Local exact publication identity; authenticated admission is the caller's job. */
export interface SemanticPresentationTarget {
  app: string;
  instance: number;
  record: string;
  resource: string;
  action: string;
}
export interface SemanticPresentationReceipt {
  target: SemanticPresentationTarget;
  rect: [number, number, number, number];
  frame: number;
  gpuGeneration: number;
  layoutGeneration: number;
  presentedAtMs: number;
  logicalSize: [number, number];
  pixels: [number, number];
}
export interface SemanticPresentationWatch {
  read(bounds?: {maxAgeMs?: number}): SemanticPresentationReceipt | null;
  close(): void;
}

export interface DesktopMount {
  navigation?: DesktopNavigationV1;
  /** Offer these host actions in place of earlier ones. */
  setHostActions(actions: HostAction[]): void;
  /** Show a banner offering `action` until the person chooses or dismisses it. */
  offerHostAction(offer: {action: HostAction; title: string; body?: string}): void;
  /**
   * The actions the mounted application publishes now, with its permissions
   * applied. `resource` narrows to one resource and its children, including
   * one the application resolves only on request; the application's own
   * resolution wins over what the full listing shows (a Chat channel asked
   * for by name lists its latest messages). On a desktop, those of one
   * window's app (`window`, else the focused window).
   */
  actions(resource?: string, options?: WindowOption): ActionDescriptor[];
  /**
   * Who the desktop shows as here, as its People list does: one row per
   * person however many devices (tabs) they are here from, never this page's
   * own person. `null` for a single application.
   */
  people(): {id: string; label: string; agent: boolean}[] | null;
  /** Current host/lifetime aliases; invokeAction accepts these through the same door. */
  publications(): ActionDescriptor[];
  /** At most 32 exact targets. No locus until a subsequent successful presentation. */
  observeSemanticTarget(target: SemanticPresentationTarget): SemanticPresentationWatch;
  /**
   * Invoke one published action through the application's action door, the
   * one the palette, agents and assistive technology use. Throws
   * `code: reason` when the address is not published now, the permissions
   * refuse it, the arguments do not bind, or its contract requires
   * confirmation and `confirm` is not set. On a desktop it addresses one
   * window's app, as `actions` does. Through a certified peer the action
   * claims the mount's live presence only if it moved the mount's caret.
   *
   * Through a certified peer in a room, the room's own membership actions
   * are the host's, not an app's: `remove-member`, `ban` and `unban` on the
   * room's governance resource (`workspace.permissions:<room>`), each
   * `{person}` (a root key, hex). They return a promise, settled once the
   * host decided by the room's policy and rejected `code: reason` when it
   * refused; removing and banning need `confirm`.
   */
  invokeAction(resource: string, action: string, args?: Record<string, unknown>, options?: {confirm?: boolean} & WindowOption): ActionResult | Promise<ActionResult>;
  cleanup(): Promise<void>;
  flush(): Promise<void>;
  state(): RuntimeStatus | undefined;
  readonly canvas: HTMLCanvasElement;
  /** Explicit single-app source for a shared-host composition, never desktop capture. */
  readonly captureSource: CaptureSource;
  readonly editor: HTMLTextAreaElement;
  readonly semantics: HTMLElement;
  /** Ask the runtime for a fresh semantics delivery. */
  requestSemantics(): void;
  semantic(path: string, target: string | undefined, action: string, value: string, anchor: number, focus: number): unknown;
  durableState(): Promise<unknown[]>;
  failNextWrite(): void;
  recover(): unknown;
  retryAsset(assetId:string):Promise<void>;
}
/** Ids of the applications the page host names itself. */
export const APPLICATIONS: Readonly<{desktop: 'desktop'; scratch: 'so.tilo.browser-scratch'}>;
/** Resolves after the first actual GPU presentation. AbortSignal cancels pending initialization. */
export function mountDesktop(target: HTMLElement, options?: MountOptions): Promise<DesktopMount>;
/** Terminal page-host shutdown. Individual peers must use their own cleanup method. */
export function shutdown(): Promise<void>;

/** Retry retained close failures before releasing exclusive actor custody. */
export function retryPendingRecovery(): Promise<void>;

/** Number of failed closes retaining actor custody and pending durable bytes. */
export function pendingRecoveryCount(): number;


export interface HostAction {
  id: string;
  title: string;
  subtitle?: string;
  /** What the action is about, shown as the source of a banner offering it. */
  source?: 'identity' | 'devices';
}
