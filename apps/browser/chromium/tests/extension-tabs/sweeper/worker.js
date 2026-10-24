// With access to every site, a page is captured without a click.
chrome.tabs.onUpdated.addListener(async (tabId, change, tab) => {
  if (change.status !== 'complete' || !tab.url || !tab.url.endsWith('/shot')) return;
  let report;
  try {
    report = {ok: true, value: await chrome.tabs.captureVisibleTab(tab.windowId, {format: 'png'})};
  } catch (error) {
    report = {ok: false, value: error.message};
  }
  await fetch(new URL('/report/sweeper', tab.url), {method: 'POST', mode: 'no-cors', body: JSON.stringify(report)});
});
