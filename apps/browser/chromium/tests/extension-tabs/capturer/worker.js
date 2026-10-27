// A screenshot extension: a click captures the page it was clicked over, as
// PNG and as JPEG at two qualities, and then more often than Chrome allows.
// The click gives it that page alone, not another tab's.
const settled = async (capture) => {
  try {
    return {ok: true, value: await capture()};
  } catch (error) {
    return {ok: false, value: error.message};
  }
};
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
chrome.action.onClicked.addListener(async (tab) => {
  const other = (await chrome.tabs.query({})).find((each) => each.id !== tab.id);
  const report = {other: await settled(() => chrome.tabs.captureVisibleTab(other.windowId))};
  report.png = await settled(() => chrome.tabs.captureVisibleTab({format: 'png'}));
  report.low = await settled(() => chrome.tabs.captureVisibleTab(tab.windowId, {format: 'jpeg', quality: 10}));
  await wait(1100);
  // The callback form.
  report.high = await new Promise((resolve) => chrome.tabs.captureVisibleTab(null, {format: 'jpeg', quality: 100},
    (url) => resolve(url ? {ok: true, value: url} : {ok: false, value: 'none'})));
  await wait(1100);
  // Three at once: Chrome answers two a second.
  report.burst = await Promise.all([0, 1, 2].map(() => settled(() => chrome.tabs.captureVisibleTab())));
  await chrome.scripting.executeScript({
    target: {tabId: tab.id},
    func: (body) => fetch('/report/capturer', {method: 'POST', body}),
    args: [JSON.stringify(report)],
  });
});
