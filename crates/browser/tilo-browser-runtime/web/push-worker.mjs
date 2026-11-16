/*
 * The page's service worker, which only shows what the person's host pushes
 * them while no Tilo of theirs is open: an agent waits for their answer
 * (web-push.mjs subscribes the browser; the host's `web_push` sends). A click
 * brings an open Tilo forward, or opens one, where the question waits.
 *
 * A classic script, not a module, so every browser that pushes registers it.
 * It caches nothing and answers no request: the page loads as it always does.
 */

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));

/** What the host pushed, read as the notification it describes. */
function pushed(event) {
  try {
    const said = event.data?.json() ?? {};
    return {
      title: typeof said.title === 'string' && said.title ? said.title : 'An agent needs you',
      body: typeof said.body === 'string' ? said.body : 'Open Tilo to answer.',
      tag: typeof said.tag === 'string' && said.tag ? said.tag : 'tilo-question',
      url: typeof said.url === 'string' ? said.url : '/',
    };
  } catch {
    return {title: 'An agent needs you', body: 'Open Tilo to answer.', tag: 'tilo-question', url: '/'};
  }
}

self.addEventListener('push', event => {
  const {title, body, tag, url} = pushed(event);
  event.waitUntil(self.registration.showNotification(title, {
    body, tag, renotify: true, requireInteraction: true,
    icon: new URL('icon.svg', self.registration.scope).href,
    data: {url: new URL(url, self.registration.scope).href},
  }));
});

self.addEventListener('notificationclick', event => {
  event.notification.close();
  const target = event.notification.data?.url ?? self.registration.scope;
  event.waitUntil((async () => {
    const open = await self.clients.matchAll({type: 'window', includeUncontrolled: true});
    const here = open.find(client => client.url.startsWith(self.registration.scope));
    if (here) return here.focus();
    return self.clients.openWindow(target);
  })());
});
