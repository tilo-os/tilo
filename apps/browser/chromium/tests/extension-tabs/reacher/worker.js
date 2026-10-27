// Content scripts on every site, and no host permission or activeTab: as in
// Chrome, a click gives it no tab's address, and no page to script or capture.
let seen;
const page = new Promise((resolve) => { seen = resolve; });
chrome.runtime.onMessage.addListener((message) => {
  if (message && message.origin) seen(message);
});
const settled = async (call) => {
  try {
    return {ok: true, value: await call()};
  } catch (error) {
    return {ok: false, value: error.message};
  }
};
const shown = (tab) => ['url', 'title', 'favIconUrl', 'pendingUrl'].filter((key) => key in tab);
chrome.action.onClicked.addListener(async (tab) => {
  const where = await page;
  const tabs = await chrome.tabs.query({});
  const report = {
    content: where.path,
    found: tabs.some((each) => each.id === tab.id),
    clicked: shown(tab),
    shown: tabs.flatMap(shown),
    by_url: (await chrome.tabs.query({url: '<all_urls>'})).length,
    script: await settled(() => chrome.scripting.executeScript({target: {tabId: tab.id}, func: () => document.title})),
    capture: await settled(() => chrome.tabs.captureVisibleTab()),
  };
  // Without access to the page it reports from here, where it may not read
  // the answer.
  await fetch(where.origin + '/report/reacher', {method: 'POST', mode: 'no-cors', body: JSON.stringify(report)});
});
