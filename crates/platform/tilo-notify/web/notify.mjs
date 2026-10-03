// The browser backend of tilo-notify (src/web.rs): the Notifications API
// while the page is open. Rust calls these while the app handles an input
// event, so a permission prompt runs inside that gesture's activation. The
// last parameter of each is the environment, replaced by tests.

const shown=new Map();

/** `granted`, `default` (not asked yet), `denied`, or `unavailable`. */
export function notificationPermission({Notification:N=globalThis.Notification}={}){
  return typeof N==='function'?N.permission:'unavailable';
}

/** Show now; the site must already hold permission. A tag replaces the
 * notification shown under it. */
export function showNotification(tag,title,body,{Notification:N=globalThis.Notification}={}){
  if(typeof N!=='function')throw Error('Notifications are unavailable in this browser');
  if(N.permission!=='granted')throw Error('Notifications are not permitted for this site');
  shown.get(tag)?.close();
  const notification=new N(title,{body,tag});
  shown.set(tag,notification);
  notification.addEventListener?.('close',()=>{if(shown.get(tag)===notification)shown.delete(tag);});
}

/** Ask for permission, then show. Rejects when the user refuses. */
export async function requestNotification(tag,title,body,env={}){
  const N=env.Notification??globalThis.Notification;
  if(typeof N!=='function')throw Error('Notifications are unavailable in this browser');
  const permission=await N.requestPermission();
  if(permission!=='granted')throw Error(`Notification permission ${permission}`);
  showNotification(tag,title,body,{Notification:N});
}

export function withdrawNotification(tag){
  const notification=shown.get(tag);
  shown.delete(tag);
  notification?.close();
}
