// With access to every site, a page is captured without activeTab.
chrome.action.onClicked.addListener(async (tab) => {
  let report;
  try {
    report = {ok: true, value: await chrome.tabs.captureVisibleTab(tab.windowId, {format: 'png'})};
  } catch (error) {
    report = {ok: false, value: error.message};
  }
  await chrome.scripting.executeScript({
    target: {tabId: tab.id},
    func: (body) => fetch('/report/sweeper', {method: 'POST', body}),
    args: [JSON.stringify(report)],
  });
});
