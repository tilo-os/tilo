chrome.runtime.onMessage.addListener((message,sender,respond)=>{
  if(message.type!=="probe") return;
  chrome.storage.local.get({count:0}).then(async ({count})=>{
    await chrome.storage.local.set({count:count+1});
    respond({worker:true,count:count+1,id:chrome.runtime.id});
  });
  return true;
});
