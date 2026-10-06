/** One explicit resource navigation. This owns no identity registry or admission. */
import {openResourceLink, parseResourceLink} from './resource-link.mjs';

export async function closeResourceOperation(operation) {
  operation?.controller.abort();
  try {
    await operation?.mount?.cleanup();
  } finally {
    // A broken app teardown must still release the peer and device custody.
    try {
      await operation?.peer?.dispose();
    } finally {
      // Connection can still be pending, before withCustody wraps a peer.
      await operation?.custody?.close();
    }
  }
}

export async function openResourceOperation({operation, link, target, connect}) {
  const signal = operation.controller.signal;
  try {
    signal.throwIfAborted();
    const address = parseResourceLink(link);
    operation.peer = await connect(address, signal);
    signal.throwIfAborted();
    operation.mount = await openResourceLink({peer: operation.peer, link, target, signal});
    signal.throwIfAborted();
    return operation.mount;
  } catch (error) {
    try {
      await closeResourceOperation(operation);
    } catch (cleanupError) {
      throw new AggregateError([error, cleanupError], 'Resource navigation and cleanup failed');
    }
    throw error;
  }
}
