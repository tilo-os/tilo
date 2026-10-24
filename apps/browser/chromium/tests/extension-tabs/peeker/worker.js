// Access to one site and every tab's address is not enough to capture one.
chrome.tabs.onUpdated.addListener(async (tabId, change, tab) => {
  if (change.status !== 'complete' || !tab.url || !tab.url.endsWith('/shot')) return;
  let report;
  try {
    report = {ok: true, value: await chrome.tabs.captureVisibleTab()};
  } catch (error) {
    report = {ok: false, value: error.message};
  }
  await fetch(new URL('/report/peeker', tab.url), {method: 'POST', mode: 'no-cors', body: JSON.stringify(report)});
});
