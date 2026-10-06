/** Resolve a shared reference through the current certified room's catalogue.
 * The source provider is required: resource ids are record-local, not global.
 * This is a fresh authorized read, never a URL capability or an invocation.
 */
export async function resolveResourceReference({reference, provider, actions, current}) {
  const bounded = (value, size) => typeof value === 'string' && value.trim().length > 0
    && value.length <= size && !/[\u0000-\u001f\u007f]/u.test(value);
  if (!bounded(provider, 2048) || !provider.startsWith('record:')
    || !bounded(reference?.id, 2048) || !bounded(reference?.kind, 128)) {
    throw Error('Invalid shared resource address');
  }
  // Caller-owned mutable objects cannot change the address during authentication.
  const address = {id: reference.id, kind: reference.kind};
  if (!actions || !current()) throw Error('Shared resource requires the current admitted room');
  const page = await actions.catalogue({provider, needle: address.id, max_results: 256});
  if (!current()) throw Error('Shared resource connection changed');
  if (page.next_cursor?.length) throw Error('Shared resource lookup exceeds the catalogue budget');
  const rows = page.rows.filter(row => row.provider === provider
    && row.resource?.id === address.id && row.resource?.kind === address.kind);
  if (!rows.length) throw Error('Shared resource unavailable or not permitted');
  const app = rows[0].app_id;
  if (!app || rows.some(row => row.app_id !== app)) throw Error('Ambiguous shared resource publisher');
  // Labels and action contracts come from the authorized live publisher, never
  // from the untrusted message. Navigation still uses its own view door.
  return {provider, app, revision: page.revision, resource: rows[0].resource,
    views: rows.filter(row => row.action?.kind === 'View').map(row => row.action)};
}
