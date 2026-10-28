// Access to one site and a click, without activeTab, are not enough to capture it.
chrome.action.onClicked.addListener(async (tab) => {
  let report;
  try {
    report = {ok: true, value: await chrome.tabs.captureVisibleTab(tab.windowId, {format: 'png'})};
  } catch (error) {
    report = {ok: false, value: error.message};
  }
  await chrome.scripting.executeScript({
    target: {tabId: tab.id},
    func: (body) => fetch('/report/peeker', {method: 'POST', body}),
    args: [JSON.stringify(report)],
  });
});
