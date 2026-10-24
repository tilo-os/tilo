// Notifications, and what the worker heard of the person choosing them.
globalThis.heard = [];
chrome.notifications.onClicked.addListener((id) => heard.push(['clicked', id]));
chrome.notifications.onButtonClicked.addListener((id, index) => heard.push(['button', id, index]));
chrome.notifications.onClosed.addListener((id, byUser) => heard.push(['closed', id, byUser]));
