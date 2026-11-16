/**
 * The person is told an agent waits for them even with no Tilo open: once
 * they let this site show notifications, the browser subscribes to pushes
 * from the host the page's session is with (`peer.pushKey`), and the host
 * keeps the subscription for them (`peer.notifyAt`, as the person's root
 * where the page holds it, else this page's own device key). The service
 * worker (push-worker.mjs) shows what the host pushes.
 *
 * Nothing is asked here: the permission is the one the offer card asks for
 * (tab-attention.mjs), which says when it is given ({@link ALLOWED}).
 */

/** The event the window hears when the person lets the site notify them. */
export const ALLOWED = 'tilo-notifications-allowed';

const same = (a, b) => a?.byteLength === b?.byteLength && a.every((byte, at) => byte === b[at]);

/**
 * Subscribe this browser to the pushes of `peer`'s host, when the person
 * allowed notifications; again whenever they allow them later. Resolves to
 * whether the host keeps a subscription of this browser's.
 */
export async function notifyThisBrowser({peer, root, window: win = globalThis} = {}) {
  const subscribe = async () => {
    const workers = win.navigator?.serviceWorker;
    if (!peer?.pushKey || !peer?.notifyAt || !workers || typeof win.PushManager !== 'function') return false;
    if (win.Notification?.permission !== 'granted') return false;
    const key = new Uint8Array(await peer.pushKey(root));
    if (!key.length) return false;
    const registration = await workers.register(new URL('push-worker.mjs', import.meta.url).href);
    await workers.ready;
    let subscription = await registration.pushManager.getSubscription();
    const subscribedWith = subscription?.options?.applicationServerKey;
    if (subscription && !same(new Uint8Array(subscribedWith ?? []), key)) {
      await subscription.unsubscribe();
      subscription = null;
    }
    subscription ??= await registration.pushManager.subscribe({userVisibleOnly: true, applicationServerKey: key});
    await peer.notifyAt(root, subscription.endpoint,
      new Uint8Array(subscription.getKey('p256dh')), new Uint8Array(subscription.getKey('auth')));
    return true;
  };
  const attempt = () => subscribe().catch(error => {
    console.warn('notifications: this browser is not told when an agent needs the person', error);
    return false;
  });
  win.addEventListener?.(ALLOWED, () => { void attempt(); }, {once: true});
  return attempt();
}
