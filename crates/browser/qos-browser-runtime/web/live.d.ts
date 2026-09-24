/** LIVE_V1 client API (`session.liveV1`); see live-client.mjs. Ephemeral, never authority. */
export type LiveRecord = {app: string; record: string};
export type LiveDragPhase = 'start' | 'move' | 'end' | 'cancel';
export type LiveDrag = {
  object: {record: LiveRecord | null; id: string /* hex, 1..64 bytes */};
  phase: LiveDragPhase;
  position: [number, number];
  size: [number, number] | null;
};
export type LivePresence = {
  focus: LiveRecord | null;
  pointer: [number, number] | null;
  selection: {anchor: string; head: string} | null /* hex CRDT cursors */;
  drag: LiveDrag | null;
};
export type LivePresenceFrame = LivePresence & {seq: number; sentAtMs: number};
export type LivePreviewOp =
  | {kind: 'insert'; at: string; text: string}
  | {kind: 'delete'; at: string; len: number};
export type LiveState = 'idle' | 'authorizing' | 'connecting' | 'live' | 'reconnecting' | 'unavailable' | 'revoked' | 'closed';
export type LiveRemoteState = {
  publisher: string;
  seq: number;
  sentAtMs: number;
  receivedAtMs: number;
  focus: LiveRecord | null;
  selection: {anchor: string; head: string} | null;
  /** Interpolated one frame behind; briefly extrapolated on loss. */
  pointer: [number, number] | null;
  /** Interpolated; `snapped` for End/Cancel. */
  drag: (LiveDrag & {snapped: boolean}) | null;
  /** Released drags kept until the durable change lands or 5 s pass. */
  endedDrags: (LiveDrag & {snapped: true})[];
  stale: boolean;
  interpolationDelayMs: number;
};
type Unsubscribe = () => void;
export interface LiveV1 {
  readonly state: LiveState;
  readonly available: boolean;
  readonly publisherId: string | undefined;
  /** Merge fields (`null` clears); coalesced to one frame per animation frame, <= 120/s. */
  publishPresence(partial: Partial<LivePresence>): boolean;
  /** Ordered ephemeral text previews for a record; false when live is unavailable. */
  publishPreview(record: LiveRecord, ops: LivePreviewOp[]): boolean;
  endPreview(record: LiveRecord): void;
  /** Receive ops (previews, early signed changes) for a record. */
  watchRecord(record: LiveRecord): Unsubscribe;
  /** A durable change for the record landed: previews dropped, drags snapped. */
  durableChanged(record: LiveRecord): void;
  /** Per-frame interpolated remote state (performance.now() timeline). */
  remoteState(nowMs?: number): LiveRemoteState[];
  onState(cb: (value: {state: LiveState}) => void): Unsubscribe;
  /** Local publish timestamps (performance.now()/Date.now()) for measurement. */
  onPublish(cb: (value: {kind: 'presence' | 'preview'; seq: number; publishedAtMs: number; sentAtMs?: number; publishedWallMs?: number}) => void): Unsubscribe;
  onRemotePresence(cb: (value: {publisher: string; frame: LivePresenceFrame; receivedAtMs: number; receivedWallMs: number}) => void): Unsubscribe;
  onPreview(cb: (value: {publisher: string; record: LiveRecord; seq: number; ops: LivePreviewOp[]; receivedAtMs: number; receivedWallMs: number}) => void): Unsubscribe;
  onSignedChange(cb: (value: {publisher: string; record: LiveRecord; digest: string; result: 'admitted' | 'duplicate' | 'deferred'; receivedAtMs: number; receivedWallMs: number; admittedAtMs: number}) => void): Unsubscribe;
  onMember(cb: (value: {publisher: string; active: boolean}) => void): Unsubscribe;
  onDurable(cb: (value: {record: LiveRecord}) => void): Unsubscribe;
  stats(): Record<string, unknown>;
}
