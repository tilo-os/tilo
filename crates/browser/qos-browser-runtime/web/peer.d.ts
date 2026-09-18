import type {Application,DesktopMount,MountOptions} from './host.mjs';
export interface CertifiedRecord {app_id:string;record_id:string;heads:string[]}
export interface PeerSnapshot {
 health?:SessionHealth;
 desktop?:{version:1;mode:'session'};
 phase:'authenticating'|'admitting'|'ready'|'incomplete'|'failed'|'closed';
 room?:string;deviceId?:string;records:CertifiedRecord[];error?:string|null;
 pendingPrerequisites?:number;persistenceNeeded?:boolean;
 outgoingPending?:number;outgoingBlocked?:number;synchronization?:'acknowledged'|'pending'|'blocked';
 recoveryAvailable?:boolean;
}
export interface CertifiedPeer {
 readonly actionsV1?:CertifiedActionsV1;
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

/** Private broker API. This is deliberately not the legacy agent sendAction API. */
export interface BrokerRoute {
 app_peer:string; app_id:string; record_id:string; resource:string; action:string; revision:string|null;
}
export type ActionConfirmation={kind:'confirm'}|{kind:'phrase';phrase:string};
export interface ActionIntent extends BrokerRoute {invocation?:string;argument?:unknown;confirmation?:ActionConfirmation|null;expected?:{host:string;binding:ActionBinding;descriptor:BrokerCatalogue['surfaces'][number]['actions'][number]}}
export type PrivateInvocationUpdate=
 | {state:'accepted';invocation:string}
 | {state:'progress';invocation:string;sequence:string;phase:string;completed:number|null;total:number|null;message:string|null}
 | {state:'item';invocation:string;sequence:string;value:unknown}
 | {state:'succeeded';invocation:string;output:{value?:unknown;resources:string[]}}
 | {state:'failed';invocation:string;error:{code:string;message:string;details?:unknown;retryable:boolean}}
 | {state:'cancelled';invocation:string;message:string|null};
export interface ActionBinding {room:string;sender:string;senderRoot:string;target:string;targetRoot:string;certificate:string}
export interface PrivateInvocation {
 invocation:string;host:string;binding:ActionBinding;intent:BrokerRoute&{type:'invoke_bound';host:string;invocation:string;argument:unknown;confirmation:ActionConfirmation|null};
 state:'unknown'|'accepted'|'progress'|'item'|'succeeded'|'failed'|'cancelled';reason:string|null;
 update:PrivateInvocationUpdate|null;terminal:boolean;cancellationRequested:boolean;
}
export interface BrokerCatalogue {
 version:1;surfaces:Array<{peer_id:string;app_id:string;cluster_id:string;record_id:string;actions:Array<{
 resource:string;action:string;revision?:string;label?:string;description?:string;
 contract:{confirmation:{mode:'none'}|{mode:'confirm'}|{mode:'phrase';phrase:string};[key:string]:unknown};
 [key:string]:unknown;
 }>}>;
}
export type BrokerCapability={state:'unavailable';reason:string}|{state:'ready';host:string;binding:ActionBinding;catalogue:BrokerCatalogue};
export interface ActionSnapshot {version:1;capability:BrokerCapability;entries:PrivateInvocation[]}
export interface CertifiedActionsV1 {
 readonly version:1;
 getSnapshot():ActionSnapshot;
 /** Independent of state/health generation; private values must not be logged. */
 subscribe(listener:(snapshot:ActionSnapshot)=>void):()=>void;
 catalogue():Promise<Extract<BrokerCapability,{state:'ready'}>>;
 /** Commit full intent before send. Reusing an ID observes status; never reruns. */
 invoke(intent:ActionIntent):Promise<PrivateInvocation>;
 status(invocation:string):Promise<PrivateInvocation>;
 /** Cooperative acknowledgement; only a durable cancelled update is terminal. */
 cancel(invocation:string):Promise<PrivateInvocation>;
 recover():Promise<ActionSnapshot>;
}
