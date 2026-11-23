// Each page tells the worker it loaded, as an ad blocker's content script
// reports what it hid there.
chrome.runtime.sendMessage({path: location.pathname});
