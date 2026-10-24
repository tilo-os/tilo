// A privacy blocker as Privacy Badger, DuckDuckGo Privacy Essentials and
// Ghostery are: its rules block trackers, and it counts what they blocked
// in each tab by the tab id webRequest reports, which its popup finds again
// through the tab APIs.
const blocked = new Map();
chrome.webRequest.onBeforeRequest.addListener((details) => {
  blocked.set(details.tabId, 0);
}, {urls: ['<all_urls>'], types: ['main_frame']});
chrome.webRequest.onErrorOccurred.addListener((details) => {
  if (details.error !== 'net::ERR_BLOCKED_BY_CLIENT' || details.tabId < 0) return;
  blocked.set(details.tabId, (blocked.get(details.tabId) || 0) + 1);
}, {urls: ['<all_urls>']});
chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message.count) {
    reply(blocked.has(message.tabId) ? blocked.get(message.tabId) : null);
    return false;
  }
  if (message.allow) {
    // Off for this site: its pages load everything.
    chrome.declarativeNetRequest.updateDynamicRules({
      removeRuleIds: [1],
      addRules: [{id: 1, priority: 2, action: {type: 'allowAllRequests'},
                  condition: {urlFilter: `|${message.allow}`, resourceTypes: ['main_frame']}}],
    }).then(() => reply(true), (error) => reply(error.message));
    return true;
  }
  return false;
});
