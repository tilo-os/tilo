// A popup asks which page it was opened over, as most extensions' do, and
// opens a page beside it.
(async () => {
  const [tab] = await chrome.tabs.query({active: true, currentWindow: true});
  const win = await chrome.windows.getCurrent({populate: true});
  const own = await chrome.tabs.getCurrent();
  const all = await chrome.tabs.query({});
  const focused = await new Promise((resolve) => chrome.tabs.query({lastFocusedWindow: true}, resolve));
  const created = await chrome.tabs.create({url: new URL('/created', tab.url).href, active: false});
  document.title = 'asked:' + JSON.stringify({
    url: tab && tab.url,
    window: win.id === tab.windowId && win.tabs.length === 1,
    own: own === undefined,
    all: all.length,
    callback: focused.length,
    created: created.url,
  });
})().catch((error) => { document.title = 'failed:' + error.message; });
