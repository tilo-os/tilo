// The page's own geolocation, answered by the engine.
//
// Chromium decides a page's geolocation permission in the browser process
// before the page hears anything, and this engine has no location provider of
// its own to back a grant, so a page that asks would be refused without
// anybody being asked. So the engine takes the page's call itself: every
// getCurrentPosition and watchPosition travels to the browser process, where
// the site's question reaches the person (or whoever acts for them) as every
// other permission request does, and the answer comes back here. Only an
// allowed site ever receives a position; a refused one is told so honestly.
(() => {
  const report = globalThis.__tiloGeoReport;
  const geo = navigator.geolocation;
  if (!report || !geo || geo.__tiloEngine) return;

  // The page's calls by id, and the positions watches already received.
  const calls = new Map();
  let next = 0;

  const position = p => ({
    coords: {
      latitude: p.latitude,
      longitude: p.longitude,
      accuracy: p.accuracy,
      altitude: null,
      altitudeAccuracy: null,
      heading: null,
      speed: null,
    },
    timestamp: Date.now(),
  });
  const denied = () => {
    const error = new Error('User denied Geolocation');
    error.code = 1;
    error.PERMISSION_DENIED = 1;
    return error;
  };
  const unavailable = () => {
    const error = new Error('Position unavailable');
    error.code = 2;
    error.POSITION_UNAVAILABLE = 2;
    return error;
  };

  // The engine's answer, for a call it still holds: a watch keeps reporting
  // until the page clears it.
  globalThis.__tiloGeoAnswer = (id, allowed, given) => {
    const call = calls.get(id);
    if (!call) return;
    if (!allowed) {
      // Refused once, a watch reports no position and leaves with the answer.
      calls.delete(id);
      call.fail(given && given.unavailable ? unavailable() : denied());
      return;
    }
    if (!call.watch) calls.delete(id);
    call.ok(position(given));
  };

  const ask = (ok, fail, watch) => {
    const id = ++next;
    calls.set(id, {ok, fail, watch});
    try {
      report(JSON.stringify({id, watch}));
    } catch (e) {
      calls.delete(id);
      fail(denied());
    }
    return id;
  };

  navigator.geolocation.getCurrentPosition = function (ok, fail) {
    ask(typeof ok === 'function' ? ok : () => {}, typeof fail === 'function' ? fail : () => {}, false);
  };
  navigator.geolocation.watchPosition = function (ok, fail) {
    return ask(typeof ok === 'function' ? ok : () => {}, typeof fail === 'function' ? fail : () => {}, true);
  };
  navigator.geolocation.clearWatch = function (id) { calls.delete(id); };
  Object.defineProperty(navigator.geolocation, '__tiloEngine', {value: true});
})();
