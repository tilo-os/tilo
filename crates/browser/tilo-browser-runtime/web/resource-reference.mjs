/** Resolve a shared reference through the current certified room's catalogue.
 * The source provider is required: resource ids are record-local, not global.
 * This is a fresh authorized read, never a URL capability or an invocation.
 */
export async function resolveResourceReference({reference, provider, actions, current, scope}) {
  const bounded = (value, size) => typeof value === 'string' && value.trim().length > 0
    && value.length <= size && !/[\u0000-\u001f\u007f]/u.test(value);
  if (!bounded(provider, 2048) || !provider.startsWith('record:')
    || !bounded(reference?.id, 2048) || !bounded(reference?.kind, 128)) {
    throw Error('Invalid shared resource address');
  }
  // Caller-owned mutable objects cannot change the address during authentication.
  const address = {id: reference.id, kind: reference.kind};
  if (!actions || !current()) throw Error('Shared resource requires the current admitted room');
  const page = await actions.catalogue({provider, view_resource: address, max_results: 256});
  if (!current()) throw Error('Shared resource connection changed');
  if (scope && (!page.locator || page.locator.origin !== scope.origin
    || page.locator.room !== scope.room || page.locator.hostDevice !== scope.hostDevice
    || page.locator.provider !== provider)) throw Error('Shared resource host locator changed or unavailable');
  if (page.next_cursor?.length) throw Error('Shared resource lookup exceeds the catalogue budget');
  const rows = page.rows.filter(row => row.provider === provider
    && row.resource?.id === address.id && row.resource?.kind === address.kind);
  if (!rows.length) throw Error('Shared resource unavailable or not permitted');
  const app = rows[0].app_id;
  if (!app || rows.some(row => row.app_id !== app)) throw Error('Ambiguous shared resource publisher');
  // Labels and action contracts come from the authorized live publisher, never
  // from the untrusted message. Navigation still uses its own view door.
  return {provider, app, revision: page.revision, locator: page.locator, resource: rows[0].resource,
    availableActions: rows.map(row => row.action).filter(Boolean),
    views: rows.filter(row => row.action?.kind === 'View' && row.action.view?.open_action === row.action.address?.action).map(row => row.action)};
}

/** Explicitly open a reference in a standalone native-rendered browser app.
 * The caller selects a published view action, never a label-derived app.
 */
export async function openResourceReference({reference, provider, action, actions, current, scope, waitRecord, mount, signal}) {
  signal?.throwIfAborted();
  const span = reference?.span == null ? null : {...reference.span};
  if (span && (!Number.isSafeInteger(span.start) || !Number.isSafeInteger(span.end)
    || span.start < 0 || span.end < span.start || !['Chars', 'Bytes'].includes(span.unit))) {
    throw Error('Invalid shared resource range');
  }
  const resolved = await resolveResourceReference({reference, provider, actions, current, scope});
  signal?.throwIfAborted();
  const prefix = `record:${resolved.app}:`;
  if (!provider.startsWith(prefix)) throw Error('Shared resource record does not match its publisher');
  const declared = resolved.views.find(view => view.address?.resource === resolved.resource.id
    && view.address.action === action);
  if (!declared) throw Error('Shared resource view unavailable');
  if (span && !declared.view.range) throw Error('Shared resource ranges require a range-aware view action');
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
    const fresh = await resolveResourceReference({reference: {id: resolved.resource.id,
      kind: resolved.resource.kind}, provider, actions, current, scope});
    signal?.throwIfAborted();
    const freshView = fresh.views.find(view => view.address?.resource === resolved.resource.id
      && view.address.action === action);
    if (fresh.app !== resolved.app || !freshView) throw Error('Shared resource view unavailable');
    if (span && !fresh.availableActions.some(candidate => candidate.kind === 'View'
      && candidate.address?.resource === resolved.resource.id
      && candidate.address.action === freshView.view.range?.action)) {
      throw Error('Shared resource range view unavailable');
    }
    const local = opened.actions(resolved.resource.id);
    const view = local.find(view => view.address?.resource === resolved.resource.id
      && view.address.action === action && view.kind === 'View' && view.view?.open_action === action);
    if (!view) throw Error('Shared resource view unavailable');
    let selected = view, args = {}, invoke = action;
    if (span) {
      const range = view.view.range;
      if (!range || ['action', 'start_param', 'end_param', 'unit_param'].some(key =>
        range[key] !== declared.view.range?.[key] || range[key] !== freshView.view.range?.[key])) {
        throw Error('Shared resource range view changed');
      }
      const names = [range.start_param, range.end_param, range.unit_param];
      if (names.some(name => typeof name !== 'string' || !name.length) || new Set(names).size !== 3) {
        throw Error('Shared resource range contract invalid');
      }
      invoke = range.action;
      selected = local.find(candidate => candidate.address?.resource === resolved.resource.id
        && candidate.address.action === invoke && candidate.kind === 'View');
      const types = ['integer', 'integer', 'text'];
      if (!selected || names.some((name, i) => !selected.params?.some(p => p.name === name && p.ty === types[i]))) {
        throw Error('Shared resource range view unavailable');
      }
      args = {[names[0]]: span.start, [names[1]]: span.end, [names[2]]: span.unit};
    }
    if ((selected.input && selected.input !== 'None' && !span)
      || selected.params?.some(param => param.required && !Object.hasOwn(args, param.name))
      || (selected.contract?.confirmation?.mode && selected.contract.confirmation.mode !== 'none')) {
      throw Error('Shared resource view unavailable or requires input');
    }
    const result = span ? opened.invokeAction(resolved.resource.id, invoke, args)
      : opened.invokeAction(resolved.resource.id, invoke);
    signal?.throwIfAborted();
    if (result.state !== 'completed' || !current()) throw Error('Shared resource view did not complete');
    return opened;
  } catch (error) {
    await opened?.cleanup();
    throw error;
  }
}
