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
export interface DesktopMount {
  navigation?: DesktopNavigationV1;
  /** The mounted application; absent for the canonical desktop. */
  readonly application?: ApplicationInfo;
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
