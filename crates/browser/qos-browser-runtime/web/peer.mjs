import init,* as runtime from './pkg/qos_browser_runtime.js';
import {mountDesktop} from './host.mjs';
import {acquireStorage} from './storage.mjs';
import {createConnectPeer} from './peer-session.mjs';
export const connectPeer=createConnectPeer({init,runtime,mountDesktop,acquireStorage});
export {pendingPeerRecoveryCount,retryPendingPeerRecovery} from './peer-session.mjs';
