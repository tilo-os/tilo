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
    views: rows.filter(row => row.action?.kind === 'View' && row.action.view?.open_action === row.action.address?.action).map(row => row.action)};
}

/** Explicitly open a reference in a standalone native-rendered browser app.
 * The caller selects a published view action, never a label-derived app.
 */
export async function openResourceReference({reference, provider, action, actions, current, waitRecord, mount, signal}) {
  signal?.throwIfAborted();
  if (reference?.span != null) throw Error('Shared resource ranges require a range-aware view action');
  const resolved = await resolveResourceReference({reference, provider, actions, current});
  signal?.throwIfAborted();
  const prefix = `record:${resolved.app}:`;
  if (!provider.startsWith(prefix)) throw Error('Shared resource record does not match its publisher');
  if (!resolved.views.some(view => view.address?.resource === resolved.resource.id
    && view.address.action === action)) throw Error('Shared resource view unavailable');
  const record = provider.slice(prefix.length);
  await waitRecord(resolved.app, record);
  signal?.throwIfAborted();
  if (!current()) throw Error('Shared resource connection changed');
  let opened;
  try {
    // The collaboration wire uses empty for RecordId::package_primary().
    // Mount the canonical id explicitly; omitting it could create a document.
    opened = await mount({app: resolved.app, record: record || '@tilo:package-primary'});
    signal?.throwIfAborted();
    if (!current()) throw Error('Shared resource connection changed');
    // The mounted app resolves the same exact address again with current
    // permissions. Read metadata does not authorize a later view invocation.
    const view = opened.actions(resolved.resource.id).find(view =>
      view.address?.resource === resolved.resource.id && view.address.action === action && view.kind === 'View'
      && view.view?.open_action === action);
    if (!view || (view.input && view.input !== 'None')
      || view.params?.some(param => param.required)
      || (view.contract?.confirmation?.mode && view.contract.confirmation.mode !== 'none')) {
      throw Error('Shared resource view unavailable or requires input');
    }
    const result = opened.invokeAction(resolved.resource.id, action);
    signal?.throwIfAborted();
    if (result.state !== 'completed' || !current()) throw Error('Shared resource view did not complete');
    return opened;
  } catch (error) {
    await opened?.cleanup();
    throw error;
  }
}
