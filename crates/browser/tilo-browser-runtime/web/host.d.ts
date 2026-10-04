import type {CaptureSource} from './panorama';
export {createPanorama} from './panorama';
export type {CaptureSource, PanoramaCamera, CaptureBounds, PanoramaReceipt, PanoramaObserver} from './panorama';
import type {DesktopNavigationV1} from "./navigation";
export type * from "./navigation";
import type {CertifiedActionsV1,PrivateInvocation} from './peer';
/**
 * What a mount runs: the id of any installed application
 * (`so.tilo.word-processor`, `so.tilo.ebook`, …; see `applications()`), the
 * Cloud frontend's aliases `writer` and `chat`, or `desktop` for the canonical
 * desktop, which requires an admitted certified peer or `MountOptions.local`. An id neither installed
 * nor resolvable (see `AppResolver`) rejects the mount with
 * `application "…" is not available`.
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
  /** Media types the app opens when a mount hands it a file (`MountOptions.open`). */
  opens: string[];
}
/**
 * A file to hand the mounted app. The bytes come from `bytes`, or from `url`
 * through the same admitted path as the app's assets: `loadAsset` when the
 * mount passes one, else a same-origin fetch. `name` defaults to the URL's
 * last path segment, `mediaType` to what the name's extension implies
 * (`.epub`: `application/epub+zip`). At most 128 MiB.
 */
export type OpenFile =
  | {url: string | URL; name?: string; mediaType?: string; bytes?: never}
  | {bytes: Uint8Array | ArrayBuffer; name: string; mediaType?: string; url?: never};
/** What the app opened a handed file as (`DesktopMount.opened`, and a status). */
export interface OpenedFile {
  name: string;
  mediaType: string;
  /**
   * The resource that holds the file in the app's action surface, for
   * `actions(resource)` and `invokeAction(resource, …)`; for Books
   * `ebook.book:book:<first 32 hex of the EPUB's BLAKE3>`. Null when the app
   * reported none.
   */
  resource: string | null;
}
/**
 * An app the core does not contain, as a resolver supplies it (the registry
 * client's `resolveApp` returns this).
 */
export interface ResolvedApp {
  /** The app's side module: compiled, a Response to stream-compile, or its verified bytes. */
  module: WebAssembly.Module | Response | Uint8Array | ArrayBuffer;
  /**
   * The runtime ABI id it was built for (`tilo-runtime/2026.07+wasm.<id>`).
   * A module for another ABI is refused with `code: 'TILO_RUNTIME_ABI_MISMATCH'`;
   * the module's own `tilo.runtime_abi` section is checked either way.
   */
  abi?: string;
  /** The app's static assets: asset name to URL. */
  assets?: Map<string, string>;
}
export type AppResolver = (appId: string, options: {signal?: AbortSignal}) => ResolvedApp | Promise<ResolvedApp>;
/** A window of a local desktop: one app in its own window group. */
export interface LocalDesktopWindow {
  /** An app id or alias, linked through the resolver when the core lacks it. */
  app: Application;
  /**
   * An existing record of the app in the mount's room, such as the `record`
   * of a standalone mount with the same profile and device: the window shows
   * that document, live with the other mount. Without it the app creates a
   * new record when the window first mounts.
   */
  record?: string;
  /** The tab's title (`label` is accepted too); defaults to the app's. */
  title?: string;
  /** Select this window when the desktop first presents. */
  focused?: boolean;
}
/**
 * The page's own desktop (`app: 'desktop'` without a peer): room documents
 * the page seeds and only this mount holds, one per workspace, the first
 * being the mount's `room`. The mount is their topology owner: the commands
 * the shell binds (open, close, reorder, move, split, strips, renames) apply
 * at once. Nothing is replicated; the rooms last as long as the mount.
 * Unknown fields are refused.
 */
export type LocalDesktopAccess = 'read' | 'comment' | 'write' | 'invite' | 'admin';
export interface LocalDesktopWorkspace {
  /** The workspace's name (`label` is accepted too); defaults to `Workspace <n>`. */
  title?: string;
  /** Each strip's windows, left to right; a workspace without strips shows the launcher. */
  strips?: Array<{title?: string; windows?: LocalDesktopWindow[]}>;
  /**
   * Room membership the owner (the mount's `author`) granted, as the
   * workspace settings' People, Agents and Privacy tabs show it. Default
   * access `write`. Presentation only: the page's own permissions stay the owner's.
   */
  members?: Array<{name: string; access?: LocalDesktopAccess; agent?: boolean}>;
  /** Capabilities granted to an app: over another app's artifacts (`over`), else the whole workspace. */
  grants?: Array<{app: string; access?: LocalDesktopAccess; over?: string}>;
}
export interface LocalDesktop {
  /** Defaults to one empty workspace. At most 16 workspaces, 64 windows, and 64 members and grants a workspace. */
  workspaces?: LocalDesktopWorkspace[];
  /** More apps to link before mounting, so the launcher, search and palette offer them. */
  apps?: string[];
}
export interface RuntimeStatus {
  /** `resolving`/`linking`/`linked` report an app installed from a side module (`app`). */
  /**
   * `crashed`: an application panicked and the runtime stopped. With
   * `crashedWindow` (its tab id) the window is kept out of the room's next
   * load, which shows `error` in it; `restarting` says the page reloads.
   */
  phase?: 'resolving' | 'linking' | 'linked' | 'gpu-pending' | 'presented' | 'failed' | 'closed' | 'crashed';
  crashedWindow?: string | null;
  restarting?: boolean;
  app?: string;
  error?: string;
  persistence?: 'committed' | 'failed';
  frames?: number;
  /** Private caller observation; never a claim of replicated document completion. */
  action?: Pick<PrivateInvocation,'invocation'|'state'|'terminal'|'cancellationRequested'>;
  actionError?: string;
  /** Native semantic command receipt lifecycle; signed room state owns rendering. */
  desktopCommand?: { invocation: string; state: string; terminal: boolean; error: string | null };
  desktopErrors?: string[];
  /** The app accepted the mount's `open` file; reported before `gpu-pending`. */
  opened?: OpenedFile;
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
  /**
   * Who the mount's edits and actions are attributed to. With `agent`, an
   * agent task participant: `agent-task:<task>` (printable ASCII, no spaces).
   */
  author?: string;
  /**
   * The mount acts as an agent with this display name (1 to 64 bytes): the
   * app admits `author` as an agent task participant, as a host admits a
   * delivered task. Chat then posts as that agent (`Name · agent` beside its
   * messages, on every mount) and lets it record its work on its own
   * messages (`set-work`). A mount whose `author` is not
   * `agent-task:<task>` is refused.
   */
  agent?: string;
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
   * A file for the app to open, as it would open one the user chose. The
   * mount rejects (and `onStatus` reports `failed`) with
   * `code: 'TILO_OPEN_INVALID'` for a malformed option,
   * `'TILO_OPEN_UNSUPPORTED'` when the app does not open its media type,
   * `'TILO_OPEN_UNAVAILABLE'` when its bytes cannot be loaded, and with the
   * app's own reason when it cannot read them. Books keeps an opened EPUB
   * in its library (the record), so another mount passing this mount's
   * `record` shows the same book and annotations without `open`.
   */
  open?: OpenFile;
  /**
   * Resolves `app` when the core does not contain it; defaults to the one
   * `setAppResolver` set. Resolution starts alongside the core's download.
   * A local desktop resolves its windows' apps, its `apps`, and every app
   * `openApp` or `{kind: 'open'}` names through it.
   */
  resolveApp?: AppResolver;
  /** With `app: 'desktop'`: mount the page's own desktop, seeded thus. */
  local?: LocalDesktop;
  /**
   * The desktop's Invite (its launchpad card and the people control in its
   * top bar) calls this; without it the desktop offers no invitation.
   */
  onInvite?: () => void;
  /** The workspace name a desktop this mount seeds starts with. */
  workspaceName?: string;
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
  /** What the app opened `MountOptions.open` as; undefined without `open`. */
  readonly opened?: OpenedFile;
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
   * Invoke one published action through the application's action door, the
   * one the palette, agents and assistive technology use. Throws
   * `code: reason` when the address is not published now, the permissions
   * refuse it, the arguments do not bind, or its contract requires
   * confirmation and `confirm` is not set. On a desktop it addresses one
   * window's app, as `actions` does. Through a certified peer the action
   * claims the mount's live presence only if it moved the mount's caret.
   */
  invokeAction(resource: string, action: string, args?: Record<string, unknown>, options?: {confirm?: boolean} & WindowOption): ActionResult;
  /**
   * A desktop's `navigation.dispatch({kind: 'open', app})` for the current
   * generation: link `app` if the core lacks it, then open it in a new window
   * of the active workspace, which a local desktop selects. With `record`, on
   * that existing record (a local desktop only). The window, its app and
   * record are then among `navigation.getSnapshot().entries`.
   */
  openApp(app: Application, options?: {record?: string}): Promise<void>;
  cleanup(): Promise<void>;
  flush(): Promise<void>;
  state(): RuntimeStatus | undefined;
  readonly canvas: HTMLCanvasElement;
  /** Explicit single-app source for a shared-host composition, never desktop capture. */
  readonly captureSource: CaptureSource;
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
/** Every application installed now (compiled in or linked), mountable standalone. */
export function applications(): Promise<ApplicationInfo[]>;
/**
 * Run a core from a verified source instead of the `pkg/` next to host.mjs:
 * `glue` is the self-contained glue (module namespace or a URL such as a
 * verified asset's object URL), `module` the core module. The registry
 * client's `resolveRuntime()` returns `{module, assets}` with the glue among
 * the assets. Call before the first mount; throws once a core is configured.
 */
export function configureRuntime(core: {glue: string | URL | object; module: WebAssembly.Module | Response | Uint8Array | ArrayBuffer}): void;
/** The runtime ABI id of the loaded core (loads it if needed); undefined for a core without one. */
export function runtimeAbiId(): Promise<string | undefined>;
/** The resolver mounts use when they pass no `resolveApp`. */
export function setAppResolver(resolver: AppResolver | undefined): void;
/**
 * Resolve and link apps before they are mounted, concurrently with each other
 * and with the core's download; a session restore passes every app it will
 * open. Resolves to their full ids. Rejects with an AggregateError whose
 * `failed` maps each app that could not be installed to its error, and whose
 * `installed` lists the others.
 */
export function preloadApps(appIds: string[], options?: {resolveApp?: AppResolver; signal?: AbortSignal}): Promise<string[]>;
/** A linked app's static assets (name to URL), undefined for compiled-in apps. */
export function appAssets(appId: string): Map<string, string> | undefined;
/** Terminal page-host shutdown. Individual peers must use their own cleanup method. */
export function shutdown(): Promise<void>;

/** Retry retained close failures before releasing exclusive actor custody. */
export function retryPendingRecovery(): Promise<void>;

/** Number of failed closes retaining actor custody and pending durable bytes. */
export function pendingRecoveryCount(): number;

/** Actual retained Rust component reports, including captured view envelopes. */
export function pendingComponentCloseCount(): number;
