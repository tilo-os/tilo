// A translator's and a clipper's calls on the page a click gave it: its
// language and zoom, and a badge, title, popup and state for that tab alone.
const settle = (promise) => promise.then((value) => ({value}), (error) => ({error: error.message}));
let clicks = 0;
chrome.action.onClicked.addListener(async (tab) => {
  clicks += 1;
  const report = {clicks};
  if (clicks === 1) {
    report.language = await chrome.tabs.detectLanguage(tab.id);
    report.zoom = await chrome.tabs.getZoom(tab.id);
    await chrome.action.setBadgeText({tabId: tab.id, text: '7'});
    await chrome.action.setTitle({tabId: tab.id, title: 'Clipped'});
    await chrome.action.setBadgeBackgroundColor({tabId: tab.id, color: '#2a6'});
    await chrome.action.setPopup({tabId: tab.id, popup: 'clipped.html'});
    await chrome.action.disable(tab.id);
    report.missing = (await settle(chrome.action.setBadgeText({tabId: 987654, text: 'x'}))).error;
  }
  report.badge = await chrome.action.getBadgeText({tabId: tab.id});
  report.global = await chrome.action.getBadgeText({});
  report.title = await chrome.action.getTitle({tabId: tab.id});
  report.popup = await chrome.action.getPopup({tabId: tab.id});
  report.enabled = await chrome.action.isEnabled(tab.id);
  await new Promise((resolve) => chrome.action.getBadgeText({tabId: tab.id}, (text) => { report.callback = text; resolve(); }));
  await chrome.scripting.executeScript({
    target: {tabId: tab.id},
    func: (report) => { document.title = 'marked:' + report; },
    args: [JSON.stringify(report)],
  });
});
