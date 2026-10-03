// The browser backend of tilo-location (src/web.rs): one Geolocation fix.
// The browser shows its own permission prompt. The last parameter is the
// environment, replaced by tests.

/** Resolves `{lat,lon,accuracy}`; rejects with a user-facing reason. */
export function locate(timeoutMs,{navigator:nav=globalThis.navigator}={}){
  return new Promise((resolve,reject)=>{
    const geo=nav?.geolocation;
    if(!geo){reject(Error('This browser has no location service'));return;}
    geo.getCurrentPosition(
      position=>resolve({lat:position.coords.latitude,lon:position.coords.longitude,accuracy:position.coords.accuracy??0}),
      error=>reject(Error(error?.code===1?'permission denied':error?.code===3?'timed out waiting for a location fix':(error?.message||'no location fix available'))),
      {enableHighAccuracy:true,timeout:timeoutMs,maximumAge:0},
    );
  });
}
