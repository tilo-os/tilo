document.title="content script ran";
chrome.runtime.sendMessage({type:"probe"},reply=>{document.title=JSON.stringify({content:true,...reply});});
