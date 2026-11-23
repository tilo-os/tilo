const port = chrome.runtime.connect({name: 'burst'});
port.onMessage.addListener((answer) => { document.title = 'burst:' + JSON.stringify(answer); });
port.onDisconnect.addListener(() => { document.title = 'burst:closed'; });
for (let at = 0; at < 300; ++at) port.postMessage(at);
