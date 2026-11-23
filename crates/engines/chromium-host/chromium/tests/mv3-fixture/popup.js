chrome.storage.local.get({count:0}).then(({count})=>{document.querySelector('#result').textContent=`Worker visits: ${count}`;document.title=`extension-action-${count}`;});
