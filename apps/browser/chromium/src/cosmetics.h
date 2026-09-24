#pragma once
#include "blocker.h"
#include "include/cef_v8.h"

namespace qos {
// This callback is passed directly into a private JS closure, never installed as
// a global binding. Its URL is fixed by the owning CEF context, not page input.
class CosmeticQuery final : public CefV8Handler {
 public:
  CosmeticQuery(std::shared_ptr<Blocker> blocker,std::string url):blocker_(std::move(blocker)),url_(std::move(url)) {}
  bool Execute(const CefString&,CefRefPtr<CefV8Value>,const CefV8ValueList& args,CefRefPtr<CefV8Value>& result,CefString& exception) override {
    if(args.size()!=2 || !args[0]->IsString() || !args[1]->IsString()) { exception="Invalid cosmetic query";return true; }
    auto resources=blocker_->Cosmetics(url_,args[0]->GetStringValue(),args[1]->GetStringValue());
    if(!resources) { exception="Native cosmetic matcher failed";return true; }
    auto value=CefValue::Create();value->SetDictionary(resources);
    result=CefV8Value::CreateString(CefWriteJSON(value,JSON_WRITER_DEFAULT));return true;
  }
 private:
  std::shared_ptr<Blocker> blocker_;std::string url_;
  IMPLEMENT_REFCOUNTING(CosmeticQuery);
};
// Generic selectors need the actual class/id tokens, including subsequently
// inserted nodes. Queries are batched and bounded; rules remain renderer-local.
inline constexpr char kCosmetics[]=R"JS(
(function(query) {
  // CSSOM-created sheets are independent of the page's style-src CSP. No page
  // policy is relaxed, and existing adopted sheets are preserved on cleanup.
  const sheet=new CSSStyleSheet();
  document.adoptedStyleSheets=[...document.adoptedStyleSheets,sheet];
  let previousCSS='';
  let timer=0, stopped=false;
  const selectors=new Set();
  function update() {
    timer=0;if(stopped)return;
    const classes=new Set(),ids=new Set();let classBytes=0,idBytes=0;
    for(const node of document.querySelectorAll('[class],[id]')) {
      for(const name of node.classList) if(!classes.has(name) && name.length<1024 && classes.size<10000 && classBytes+name.length<120000) { classes.add(name);classBytes+=name.length; }
      if(node.id && !ids.has(node.id) && node.id.length<1024 && ids.size<10000 && idBytes+node.id.length<120000) { ids.add(node.id);idBytes+=node.id.length; }
    }
    const resource=JSON.parse(query(JSON.stringify([...classes]),JSON.stringify([...ids])));
    for(const selector of resource.hide_selectors) {
      // A malformed third-party selector must not invalidate the whole sheet.
      if(selectors.has(selector))continue;
      try { document.querySelector(selector);selectors.add(selector); } catch(_) {}
    }
    if(!document.adoptedStyleSheets.includes(sheet)) document.adoptedStyleSheets=[...document.adoptedStyleSheets,sheet];
    const css=[...selectors].map(s=>s+' { display: none !important; }').join('\n');
    if(previousCSS!==css) { sheet.replaceSync(css);previousCSS=css; }
  }
  const observer=new MutationObserver(()=>{
    if(!timer)timer=setTimeout(update,25);
  });
  observer.observe(document,{subtree:true,childList:true,attributes:true,attributeFilter:['class','id']});
  update();
  return function(){stopped=true;observer.disconnect();clearTimeout(timer);document.adoptedStyleSheets=document.adoptedStyleSheets.filter(candidate=>candidate!==sheet)};
})
)JS";
}
