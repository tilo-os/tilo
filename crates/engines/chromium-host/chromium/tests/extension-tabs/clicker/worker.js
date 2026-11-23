// A toolbar button without a popup: a click marks the page it was clicked
// over, which activeTab lets it script.
chrome.action.onClicked.addListener(async (tab) => {
  const [result] = await chrome.scripting.executeScript({
    target: {tabId: tab.id},
    func: (url) => { document.title = 'clicked:' + (location.href === url); return document.title; },
    args: [tab.url],
  });
  await chrome.scripting.executeScript({
    target: {tabId: tab.id},
    func: (seen) => { document.title += '|' + seen; },
    args: [result.result],
  });
});
