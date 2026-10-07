/** Navigation addresses, never bearer grants. All resolution happens after
 * certification, room admission and a fresh permission-filtered catalogue. */
const bounded = (value, max) => typeof value === 'string' && value.length > 0
  && value.length <= max && !/[\u0000-\u001f\u007f]/u.test(value);
const safeOrigin = value => {
  const url = new URL(value);
  if (url.origin !== value || url.username || url.password
    || !(url.protocol === 'https:' || (url.protocol === 'http:'
      && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)))) {
    throw Error('Resource link requires an HTTPS host origin');
  }
  return url.origin;
};
export function resourceLinkAddress(value) {
  if (value?.version !== 1 || !bounded(value.room, 128)
    || !/^[a-f0-9]{64}$/u.test(value.hostDevice ?? '')
    || !bounded(value.provider, 2048) || !value.provider.startsWith('record:')
    || !bounded(value.reference?.id, 2048) || !bounded(value.reference?.kind, 128)
    || !bounded(value.action, 128)) throw Error('Invalid resource link');
  const span = value.reference.span;
  if (span != null && (!Number.isSafeInteger(span.start) || !Number.isSafeInteger(span.end)
    || span.start < 0 || span.end < span.start || !['Chars', 'Bytes'].includes(span.unit))) {
    throw Error('Invalid resource link range');
  }
  // No caller labels, credentials, invitations, or extra fields cross the link.
  return {version: 1, origin: safeOrigin(value.origin), room: value.room,
    hostDevice: value.hostDevice, provider: value.provider, action: value.action,
    reference: {kind: value.reference.kind, id: value.reference.id,
      ...(span == null ? {} : {span: {start: span.start, end: span.end, unit: span.unit}})}};
}
export function parseResourceLink(link) {
  const url = new URL(link);
  if (!url.hash.startsWith('#resource=') || url.hash.length > 24_000) throw Error('Invalid resource link');
  return resourceLinkAddress(JSON.parse(decodeURIComponent(url.hash.slice(10))));
}
export async function createResourceLink({peer, page, reference, provider, action}) {
  const address = resourceLinkAddress({...peer.resourceScope, version: 1, reference, provider, action});
  const resolved = await peer.resolveResource(address.reference, address.provider);
  const scope = peer.resourceScope;
  if (!scope || scope.origin !== address.origin || scope.room !== address.room
    || scope.hostDevice !== address.hostDevice) throw Error('Resource link connection changed');
  const locator = resolved.locator;
  if (!locator || locator.origin !== address.origin || locator.room !== address.room
    || locator.hostDevice !== address.hostDevice || locator.provider !== address.provider) {
    throw Error('Resource link requires a current host-issued locator');
  }
  const view = resolved.views.find(view => view.address.action === address.action);
  if (!view || (address.reference.span != null && !view.view.range)) throw Error('Resource view unavailable');
  const url = new URL(page);
  safeOrigin(url.origin);
  if (url.username || url.password) throw Error('Invalid resource link page');
  url.search = '';
  url.hash = `resource=${encodeURIComponent(JSON.stringify(address))}`;
  return url.href;
}
export async function openResourceLink({peer, link, target, signal}) {
  const address = parseResourceLink(link);
  signal?.throwIfAborted();
  await peer.whenReady({signal});
  signal?.throwIfAborted();
  const scope = peer.resourceScope;
  if (!scope || scope.origin !== address.origin || scope.room !== address.room
    || scope.hostDevice !== address.hostDevice) throw Error('Resource link belongs to another host or room');
  return peer.openResource(target, address.reference, address.provider, address.action, {signal});
}
