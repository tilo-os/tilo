// A content script on every page tells the worker where it runs.
chrome.runtime.sendMessage({origin: location.origin, path: location.pathname});
