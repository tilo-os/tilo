/** A mount-owned source; only DesktopMount.captureSource creates public handles. */
declare const sourceBrand: unique symbol;
export interface CaptureSource {readonly [sourceBrand]: true}
export interface PanoramaCamera {cx: number; cy: number; scale: number}
export interface CaptureBounds {maxAgeMs?: number; maxSkewMs?: number}
export interface PanoramaPointer {
  publisher: string;
  sequence: number;
  receivedAtMs: number;
  screen: [number, number];
  kind: 'observed-pointer';
}
export interface PanoramaPaneReceipt {
  index: number;
  app: string;
  record: string;
  frame: number;
  gpuGeneration: number;
  /** Present on runtime hosts exposing the semantic presentation seam. */
  layoutGeneration?: number;
  presentedAtMs: number;
  logicalSize: [number, number];
  pixels: [number, number];
  presenceAvailable: boolean;
  transform: [number, number, number, number, number, number];
  pointers: PanoramaPointer[];
}
export interface PanoramaReceipt {
  version: 1;
  provenance: 'shared-host composition of retained mounts';
  sequence: number;
  observedAtMs: number;
  camera: PanoramaCamera;
  size: [number, number];
  panes: PanoramaPaneReceipt[];
}
export interface PanoramaObserver {
  /** Null while hidden; throws and blanks output on an unavailable source. */
  render(camera: PanoramaCamera, bounds?: CaptureBounds): PanoramaReceipt | null;
  /** Exactly one PNG encode may be outstanding. */
  png(camera: PanoramaCamera, bounds?: CaptureBounds): Promise<{blob: Blob; receipt: PanoramaReceipt}>;
  setVisible(visible: boolean): void;
  close(): void;
}
export function createPanorama(options: {
  canvas: HTMLCanvasElement;
  sources: Array<{source: CaptureSource; rect: [number, number, number, number]}>;
  width?: number;
  height?: number;
  now?: () => number;
}): PanoramaObserver;
