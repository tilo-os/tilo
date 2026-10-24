// A screenshot extension: a click captures the page it was clicked over, as
// PNG and as JPEG at two qualities, and then more often than Chrome allows.
// Before the click it may not capture the page at all.
let before = null;
const settled = async (capture) => {
  try {
    return {ok: true, value: await capture()};
  } catch (error) {
    return {ok: false, value: error.message};
  }
};
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
chrome.tabs.onUpdated.addListener(async (tabId, change) => {
  if (change.status === 'complete' && !before) before = await settled(() => chrome.tabs.captureVisibleTab());
});
chrome.action.onClicked.addListener(async (tab) => {
  const report = {before};
  report.png = await settled(() => chrome.tabs.captureVisibleTab({format: 'png'}));
  report.low = await settled(() => chrome.tabs.captureVisibleTab(tab.windowId, {format: 'jpeg', quality: 10}));
  await wait(1100);
  // The callback form.
  report.high = await new Promise((resolve) => chrome.tabs.captureVisibleTab(null, {format: 'jpeg', quality: 100},
    (url) => resolve(url ? {ok: true, value: url} : {ok: false, value: 'none'})));
  await wait(1100);
  // Three at once: Chrome answers two a second.
  report.burst = await Promise.all([0, 1, 2].map(() => settled(() => chrome.tabs.captureVisibleTab())));
  await fetch(new URL('/report/capturer', tab.url), {method: 'POST', mode: 'no-cors', body: JSON.stringify(report)});
});
