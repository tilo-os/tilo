import type {DesktopNavigationV1} from "./navigation";
export type * from "./navigation";
import type {CertifiedActionsV1,PrivateInvocation} from './peer';
/**
 * What a mount runs: the id of any installed application
 * (`so.tilo.word-processor`, `so.tilo.ebook`, …; see `applications()`), the
 * Cloud frontend's aliases `writer` and `chat`, or `desktop` for the canonical
 * desktop, which requires an admitted certified peer. An id neither installed
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
export interface RuntimeStatus {
  /** `resolving`/`linking`/`linked` report an app installed from a side module (`app`). */
  phase?: 'resolving' | 'linking' | 'linked' | 'gpu-pending' | 'presented' | 'failed' | 'closed';
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
  author?: string;
  instance?: number;
  record?: string;
  newDocument?: boolean;
  mac?: boolean;
  signal?: AbortSignal;
  onStatus?: (status: RuntimeStatus) => void;
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
   */
  resolveApp?: AppResolver;
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
