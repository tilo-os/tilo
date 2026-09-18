import type {Application,DesktopMount,MountOptions} from './host.mjs';
export interface CertifiedRecord {app_id:'dev.qos.word-processor'|'dev.qos.chat';record_id:string;heads:string[]}
export interface PeerSnapshot {
 health?:SessionHealth;
 phase:'authenticating'|'admitting'|'ready'|'incomplete'|'failed'|'closed';
 room?:string;deviceId?:string;records:CertifiedRecord[];error?:string|null;
 pendingPrerequisites?:number;persistenceNeeded?:boolean;
 outgoingPending?:number;outgoingBlocked?:number;synchronization?:'acknowledged'|'pending'|'blocked';
 recoveryAvailable?:boolean;
}
export interface CertifiedPeer {
 getSnapshot():PeerSnapshot;
 subscribe(listener:(snapshot:PeerSnapshot)=>void):()=>void;
 mountDesktop(target:HTMLElement,options:Pick<MountOptions,'app'|'instance'|'signal'|'onStatus'>&{onSurface?:(surfaceId:string)=>void;onMount?:(mount:DesktopMount)=>void}):Promise<DesktopMount>;
 recover(request:RecoveryRequest):Promise<RecoveryResult>;
 reconnect():Promise<void>;
 retryPersistence():Promise<void>;
 dispose():Promise<void>;
}
/** Resolves after authenticated join; observe ready for admitted durable records. */
export function connectPeer(options?:{enrollmentBaseUrl?:string;signal?:AbortSignal;onStatus?:(snapshot:PeerSnapshot)=>void}):Promise<CertifiedPeer>;

export function pendingPeerRecoveryCount():number;
export function retryPendingPeerRecovery():Promise<void>;

export type EpochMs = number;
export interface RecordRef { roomId: string; appId: string; recordId: string }
export interface StorageRevision { epoch: string; revision: string }
export interface RecordBasis { heads: string[] }

export type FailureScope =
  | { kind: "connection" }
  | { kind: "admission"; record?: RecordRef }
  | { kind: "storage"; record?: RecordRef }
  | { kind: "renderer"; surfaceId: string }
  | { kind: "asset"; surfaceId: string; assetId: string }
  | { kind: "permission"; record?: RecordRef };

export type FailureCode =
  | "transport-unavailable" | "transport-ended" | "admission-timeout"
  | "checkpoint-expired" | "prerequisite-missing" | "crypto-unavailable"
  | "certificate-expired" | "identity-revoked" | "signature-rejected"
  | "certificate-rejected" | "protocol-rejected" | "runtime-unavailable"
  | "record-install-failed" | "storage-quota" | "storage-denied"
  | "storage-transaction-failed" | "asset-unavailable"
  | "gpu-unavailable" | "gpu-device-lost" | "renderer-initialization-failed"
  | "permission-required";

export interface SessionFailure {
  id: string;
  code: FailureCode;
  scope: FailureScope;
  severity: "notice" | "error";
  message: string;                 // safe product copy, no private data/stack
  detailId?: string;               // safe diagnostic correlation only
  recovery: RecoveryRequest[];     // producer-authorized applicable controls
  occurredAtMs: EpochMs;
}

export type RecoveryRequest =
  | { kind: "retry-connection" }
  | { kind: "refresh-checkpoint"; record?: RecordRef }
  | { kind: "retry-saving"; record?: RecordRef }
  | { kind: "retry-desktop"; surfaceId: string }
  | { kind: "retry-asset"; surfaceId: string; assetId: string }
  | { kind: "reauthenticate" }
  | { kind: "review-permissions"; failureId: string };

export interface RecoveryOperation {
  id: string;
  request: RecoveryRequest;
  state: "pending" | "succeeded" | "failed" | "requires-user";
  failureId?: string;
}
export interface RecoveryResult { operationId: string }

export interface RecordHealth {
  record: RecordRef;
  admission: "waiting" | "admitted" | "unavailable";
  current: RecordBasis | null;
  editing: {
    mode: "online" | "offline-queued" | "read-only" | "blocked";
    reasonCode?: FailureCode;
  };
  local: {
    state: "unknown" | "pending" | "committing" | "durable" | "failed";
    durableBasis: RecordBasis | null;
    recordRevision: StorageRevision | null;
    journalRevision: StorageRevision | null;
    pendingWrites: number;
    pendingBytes: number;
    committedAtMs: EpochMs | null;
  };
  outgoing: { pendingOperations: number; durableOperations: number };
  acknowledgement: {
    state: "unknown" | "pending" | "covered" | "failed";
    targetPeerId: string | null;
    coveredBasis: RecordBasis | null;
    coversCurrentBasis: boolean;    // calculated by qOS causal coverage
    receiptId: string | null;
    observedAtMs: EpochMs | null;   // local observation, not remote wall clock
  };
  failureIds: string[];
}

export interface SessionHealth {
  version: 1;
  generation: string;
  transport: {
    phase: "connecting" | "connected" | "reconnecting" | "offline" | "failed" | "closed";
    authenticated: boolean;
    lastRelayContactAtMs: EpochMs | null;
    lastSessionPeerContactAtMs: EpochMs | null;
    retry: { attempt: number; nextAttemptAtMs: EpochMs | null; inFlight: boolean };
  };
  admission: {
    phase: "waiting" | "syncing" | "ready" | "failed";
    requiredRecords: RecordRef[];    // canonical desktop requirement, not React guesses
    pendingPrerequisites: number;
    deadlineAtMs: EpochMs | null;
  };
  records: RecordHealth[];
  surfaces: Array<{
    id: string;
    record?: RecordRef;
    phase: "waiting" | "initializing" | "presented" | "recovering" | "failed" | "closed";
    generation: string;
    failureIds: string[];
  }>;
  capabilities: {
    automaticRecovery: boolean;
    retrySaving: boolean;
    recoverRenderer: boolean;
    durablePeerAcknowledgement: boolean;
  };
  failures: SessionFailure[];
  recoveryOperations: RecoveryOperation[];
}
