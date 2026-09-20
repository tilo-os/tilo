import init,* as runtime from './pkg/qos_browser_runtime.js';
import {mountDesktop} from './host.mjs';
import {acquireStorage,createIdentityStore} from './storage.mjs';
import {createIdentityCustodian} from './identity-custody.mjs';
import {createConnectPeer} from './peer-session.mjs';
export const connectPeer=createConnectPeer({init,runtime,mountDesktop,acquireStorage,acquireIdentity:createIdentityCustodian({runtime,locks:globalThis.navigator?.locks,store:createIdentityStore()})});
export {pendingPeerRecoveryCount,retryPendingPeerRecovery} from './peer-session.mjs';
