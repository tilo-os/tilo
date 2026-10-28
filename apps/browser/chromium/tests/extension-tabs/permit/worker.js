// The worker hears what the extension gained and gave up, and scripts the
// page in front, which it may only with access to the site.
globalThis.heard = [];
chrome.permissions.onAdded.addListener((permissions) => heard.push({added: permissions}));
chrome.permissions.onRemoved.addListener((permissions) => heard.push({removed: permissions}));
globalThis.urls = async () => (await chrome.tabs.query({})).map((tab) => tab.url || null);
globalThis.script = async () => {
  const [tab] = await chrome.tabs.query({active: true, lastFocusedWindow: true});
  try {
    const [result] = await chrome.scripting.executeScript({target: {tabId: tab.id}, func: () => location.pathname});
    return result.result;
  } catch (error) {
    return 'refused: ' + error.message;
  }
};
