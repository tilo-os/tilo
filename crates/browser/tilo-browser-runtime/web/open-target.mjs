/**
 * Show a link's target in a mount, through the same doors a person's own
 * navigation uses: on the desktop the navigation `open` command focuses or
 * launches the app's window, then the resource's declared view action runs
 * through that window's action door. A target the door does not offer
 * fails with one message whether it is missing or not for this person, so
 * a link never tells a stranger what exists.
 */
export const UNAVAILABLE = 'This link points at something that is not there, or that you cannot open';

/** The view action (and arguments) that shows `resource` in view `viewId`
 * (`undefined`: the primary view), from the door's current descriptors. */
export function viewInvocation(actions, resource, viewId) {
  // A view's open action carries the view's descriptor; nothing else does.
  const views = actions.filter(action => action.address?.resource === resource.id && action.view
    && action.view.open_action === action.address.action);
  const view = viewId === undefined
    ? views.find(action => action.view.primary) ?? views[0]
    : views.find(action => action.view.id === viewId);
  if (!view) throw new Error(UNAVAILABLE);
  if (!resource.span) return {action: view.address.action, args: {}};
  const range = view.view.range;
  if (!range) throw new Error('This view cannot show a range of its text');
  const {start, end, unit} = resource.span;
  return {action: range.action, args: {[range.start_param]: start, [range.end_param]: end, [range.unit_param]: unit}};
}

/** The app of the focused window in a navigation snapshot, or null. */
export function focusedApp(snapshot) {
  const active = snapshot?.active;
  if (!active) return null;
  const same = route => route.workspace === active.workspace && route.strip === active.strip
    && route.group === active.group && route.tab === active.tab;
  return snapshot.entries?.find(entry => same(entry.route))?.app ?? null;
}

/** How many frames the desktop draws after a link's command before the
 * link may give up on its window: the desktop acts on a command in the
 * frame after it. */
const FRAMES_TO_SHOW = 3;

/**
 * Resolve once `predicate(snapshot)` holds, or reject once `timeoutMs` passed
 * and the desktop drew `FRAMES_TO_SHOW` frames since (`frames()`, the
 * mount's presented frames), so it had its chance to show it. A desktop
 * that draws slowly (a busy device, a software rasteriser compiling its
 * pipelines: a frame took seconds there) is waited for, not failed: the
 * whole page stopped after 10 s while the window was still on its way.
 */
export function untilSnapshot(navigation, predicate, {timeoutMs = 10_000, timers = globalThis, frames = () => Infinity} = {}) {
  return new Promise((resolve, reject) => {
    let stop = () => {};
    let timer;
    const from = frames();
    const expire = () => {
      if (frames() - from < FRAMES_TO_SHOW) { timer = timers.setTimeout(expire, timeoutMs / 10); return; }
      stop();
      reject(new Error('The desktop did not show the link in time'));
    };
    timer = timers.setTimeout(expire, timeoutMs);
    const check = snapshot => {
      if (!predicate(snapshot)) return false;
      timers.clearTimeout(timer);
      stop();
      resolve(snapshot);
      return true;
    };
    if (check(navigation.getSnapshot())) return;
    stop = navigation.subscribe(check);
  });
}

/** How often, and how long, a link waits for its resource to be offered. */
const VIEWABLE_POLL_MS = 100;
const VIEWABLE_WAIT_MS = 10_000;

/**
 * The view invocation for `target`'s resource once `mount` offers it. A
 * resource can reach the page after its app's record: a Chat message is
 * kept in a part of the chat's messages, a record that syncs on its own
 * route and that the window takes up when it arrives. Only after the wait,
 * and once the desktop drew `FRAMES_TO_SHOW` frames since it began
 * (`frames()`, as for its window), is the resource not there: the app offers
 * its views from a running window, and a desktop that draws slowly (a page
 * joining a desktop of forty windows took 16 s for a frame) is waited for.
 */
export async function untilViewable(mount, target, options,
  {timers = globalThis, waitMs = VIEWABLE_WAIT_MS, frames = () => Infinity, loading = () => false} = {}) {
  let from = frames();
  let waited = 0;
  for (;;) {
    try {
      return viewInvocation(mount.actions(target.resource.id, options), target.resource, target.view);
    } catch (error) {
      if (error.message !== UNAVAILABLE) throw error;
      // The app's window offers its views once its code is in, and that code
      // is a download of its own (tens of megabytes for the largest apps, the
      // page allows minutes for it): the wait for the views starts when it
      // arrived, not when the link asked. The desk drawing meanwhile says
      // nothing about the app.
      if (loading()) {
        waited = 0;
        from = frames();
      } else if (waited >= waitMs && !(frames() - from < FRAMES_TO_SHOW)) {
        throw error;
      }
    }
    await new Promise(done => timers.setTimeout(done, VIEWABLE_POLL_MS));
    waited += VIEWABLE_POLL_MS;
  }
}

/**
 * Show `target` (a parsed link target) in `mount`. Desktop mode needs the
 * mount's `navigation`; app mode mounted the app already.
 */
export async function openTarget({mount, navigation, target, mode, timers}) {
  if (!target.app) return;
  const frames = () => mount.state?.()?.frames ?? Infinity;
  let window;
  if (mode === 'desktop') {
    if (!navigation) throw new Error('This desktop cannot follow links');
    const snapshot = navigation.getSnapshot();
    if (focusedApp(snapshot) !== target.app) {
      await navigation.dispatch({effect: 'show-app', args: {app: target.app}});
    }
    const shown = await untilSnapshot(navigation, next => focusedApp(next) === target.app, {timers, frames});
    window = shown.active.tab;
  }
  if (!target.resource) return;
  const options = window === undefined ? undefined : {window};
  const loading = () => (mount.state?.()?.appsLoading ?? []).includes(target.app);
  const {action, args} = await untilViewable(mount, target, options, {timers, frames, loading});
  let result;
  try {
    result = mount.invokeAction(target.resource.id, action, args, options);
  } catch {
    // The door's own reason could tell "missing" from "refused".
    throw new Error(UNAVAILABLE);
  }
  if (result?.state !== 'completed' && result?.state !== 'accepted') throw new Error(UNAVAILABLE);
}
