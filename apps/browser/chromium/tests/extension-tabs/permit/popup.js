// A popup that asks for more access when the person clicks in it, as an
// extension asks for a site only once it is needed; `?drop` gives the site
// up again.
const show = (label, value) => { document.title = label + ':' + JSON.stringify(value); };
const failure = (promise) => promise.then(() => null, (error) => error.message);
const state = async () => {
  const [tab] = await chrome.tabs.query({active: true, currentWindow: true});
  return {
    all: await chrome.permissions.getAll(),
    tabs: await chrome.permissions.contains({permissions: ['tabs']}),
    site: await chrome.permissions.contains({origins: ['http://127.0.0.1/*']}),
    url: (tab && tab.url) || null,
  };
};
if (location.search === '?drop') {
  (async () => {
    const required = await failure(chrome.permissions.remove({permissions: ['storage']}));
    const removed = await chrome.permissions.remove({origins: ['http://127.0.0.1/*']});
    show('dropped', {required, removed, ...await state()});
  })().catch((error) => show('failed', error.message));
} else {
  // Asked without the person doing anything, it is refused.
  (async () => show('ready', {unasked: await failure(chrome.permissions.request({permissions: ['tabs']})), ...await state()}))()
    .catch((error) => show('failed', error.message));
  document.addEventListener('click', () => {
    // What the manifest does not offer is refused; what it does is asked
    // for, here in the callback form.
    const undeclared = failure(chrome.permissions.request({permissions: ['history']}));
    chrome.permissions.request({permissions: ['tabs'], origins: ['http://127.0.0.1/*']}, async (granted) => {
      const error = chrome.runtime.lastError ? chrome.runtime.lastError.message : null;
      show('asked', {granted, error, undeclared: await undeclared, ...await state()});
    });
  });
}
