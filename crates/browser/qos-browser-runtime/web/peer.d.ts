import type {Application,DesktopMount,MountOptions} from './host.mjs';
export interface CertifiedRecord {app_id:'dev.qos.word-processor'|'dev.qos.chat';record_id:string;heads:string[]}
export interface PeerSnapshot {
 phase:'authenticating'|'admitting'|'ready'|'incomplete'|'failed'|'closed';
 room?:string;deviceId?:string;records:CertifiedRecord[];error?:string|null;
 pendingPrerequisites?:number;persistenceNeeded?:boolean;
 outgoingPending?:number;outgoingBlocked?:number;synchronization?:'acknowledged'|'pending'|'blocked';
 recoveryAvailable?:boolean;
}
export interface CertifiedPeer {
 getSnapshot():PeerSnapshot;
 subscribe(listener:(snapshot:PeerSnapshot)=>void):()=>void;
 mountDesktop(target:HTMLElement,options:Pick<MountOptions,'app'|'instance'|'signal'|'onStatus'>):Promise<DesktopMount>;
 reconnect():Promise<void>;
 retryPersistence():Promise<void>;
 dispose():Promise<void>;
}
/** Resolves after authenticated join; observe ready for admitted durable records. */
export function connectPeer(options?:{enrollmentBaseUrl?:string;signal?:AbortSignal;onStatus?:(snapshot:PeerSnapshot)=>void}):Promise<CertifiedPeer>;

export function pendingPeerRecoveryCount():number;
export function retryPendingPeerRecovery():Promise<void>;
