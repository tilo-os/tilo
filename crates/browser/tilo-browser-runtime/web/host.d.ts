import type {DesktopNavigationV1} from "./navigation";
export type * from "./navigation";
import type {CertifiedActionsV1,PrivateInvocation} from './peer';
export type Application = 'writer' | 'chat' | 'desktop';
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
/** Terminal page-host shutdown. Individual peers must use their own cleanup method. */
export function shutdown(): Promise<void>;

/** Retry retained close failures before releasing exclusive actor custody. */
export function retryPendingRecovery(): Promise<void>;

/** Number of failed closes retaining actor custody and pending durable bytes. */
export function pendingRecoveryCount(): number;

/** Actual retained Rust component reports, including captured view envelopes. */
export function pendingComponentCloseCount(): number;
