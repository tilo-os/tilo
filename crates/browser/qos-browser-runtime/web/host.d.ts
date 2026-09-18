export type Application = 'writer' | 'chat';
export interface RuntimeStatus {
  phase?: 'gpu-pending' | 'presented' | 'failed' | 'closed';
  error?: string;
  persistence?: 'committed' | 'failed';
  frames?: number;
  document?: unknown;
  heads?: string[];
  [diagnostic: string]: unknown;
}
export interface MountOptions {
  app?: Application;
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
  probePixels(): Promise<unknown>;
}
/** Resolves after the first actual GPU presentation. AbortSignal cancels pending initialization. */
export function mountDesktop(target: HTMLElement, options?: MountOptions): Promise<DesktopMount>;
/** Terminal page-host shutdown. Individual peers must use their own cleanup method. */
export function shutdown(): Promise<void>;

/** Retry retained close failures before releasing exclusive actor custody. */
export function retryPendingRecovery(): Promise<void>;
