/** Native app navigation uses the same certified, permission-filtered catalogue
 * as the page. No local app reply substitutes for that host's current Read. */
export function createNativeCatalogueBridge({take, deliver, actions, current, onError = () => {}}) {
  let closed = false, scheduled = false;
  const pending = new Set();
  const active = () => !closed && current();
  const unavailable = reason => ({Err:{Unavailable:{reason}}});
  const answer = (correlation, result) => {
    if (active()) {
      try { deliver({CatalogueResponse:{correlation, result}}); }
      catch (error) { onError(error); }
    }
  };
  function pump() {
    if (!active() || scheduled) return;
    scheduled = true;
    // Runtime status callbacks hold the mount borrow; enter after they return.
    queueMicrotask(() => {
      scheduled = false;
      if (!active()) return;
      let requests;
      try { requests = take(); } catch (error) { onError(error); return; }
      for (const message of requests) {
        const request = message.CatalogueRequest;
        if (!request || !Number.isSafeInteger(request.correlation)) continue;
        const {correlation, query} = request;
        if (pending.has(correlation)) continue;
        const connection = actions();
        if (!connection || pending.size >= 16) {
          answer(correlation, unavailable('Certified catalogue unavailable or busy'));
          continue;
        }
        pending.add(correlation);
        Promise.resolve().then(() => connection.catalogue(query)).then(page => {
          answer(correlation, actions() === connection ? {Ok:page}
            : unavailable('Certified catalogue connection changed'));
        }, error => answer(correlation, unavailable(String(error?.message ?? error))))
          .finally(() => pending.delete(correlation));
      }
    });
  }
  return {pump, dispose(){closed = true;}};
}
