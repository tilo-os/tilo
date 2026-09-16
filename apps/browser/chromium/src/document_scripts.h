#pragma once
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include <functional>

namespace qos {
inline std::string ScriptSource(const std::vector<CefString>& scripts) {
  auto list=CefListValue::Create();for(size_t i=0;i<scripts.size();++i) list->SetString(i,scripts[i]);
  auto value=CefValue::Create();value->SetList(list);
  const auto json=CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString();
  // Registration is browser-wide, but actual scripts are restricted to direct
  // HTTP(S) documents. The same wrapper is used for live configuration updates;
  // its per-document value prevents load-end synchronization from running twice.
  return "(()=>{if(location.protocol!=='http:'&&location.protocol!=='https:')return;const scripts="+json+
    ";const key=Symbol.for('qos.documentScripts');const signature=JSON.stringify(scripts);if(window[key]===signature)return;window[key]=signature;for(const script of scripts){try{(0,eval)(script)}catch(e){console.error('qOS document script failed',e)}}})()";
}
class DocumentScripts final : public CefDevToolsMessageObserver {
 public:
  DocumentScripts(CefRefPtr<CefBrowser> browser,std::function<void()> ready,std::function<void()> failure)
    :browser_(browser),ready_(std::move(ready)),failure_(std::move(failure)) {}
  void Attach() {
    registration_=browser_->GetHost()->AddDevToolsMessageObserver(this);
    enabling_=true;pending_=browser_->GetHost()->ExecuteDevToolsMethod(0,"Page.enable",nullptr);
    if(!pending_) failure_();
  }
  void Detach() { registration_=nullptr;browser_=nullptr; }
  bool Busy() const { return pending_!=0 || desired_!=installed_; }
  void Configure(const std::string& source) { desired_=source;if(!pending_) Start(); }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    if(id!=pending_) return;
    pending_=0;
    if(!success) { failure_();return; }
    if(enabling_) { enabling_=false;Start();return; }
    if(removing_) { identifier_.clear();installed_.clear();Start();return; }
    auto parsed=CefParseJSON(std::string(static_cast<const char*>(result),size),JSON_PARSER_RFC);
    if(!parsed || parsed->GetType()!=VTYPE_DICTIONARY || parsed->GetDictionary()->GetType("identifier")!=VTYPE_STRING) { failure_();return; }
    identifier_=parsed->GetDictionary()->GetString("identifier");installed_=submitted_;
    if(desired_!=installed_) Start();else ready_();
  }
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override { if(browser_ && pending_) { pending_=0;failure_(); } }
 private:
  void Start() {
    if(!browser_ || desired_==installed_) { if(browser_) ready_();return; }
    auto params=CefDictionaryValue::Create();
    removing_=!identifier_.empty();
    if(removing_) params->SetString("identifier",identifier_);
    else { submitted_=desired_;params->SetString("source",submitted_); }
    pending_=browser_->GetHost()->ExecuteDevToolsMethod(0,removing_?"Page.removeScriptToEvaluateOnNewDocument":"Page.addScriptToEvaluateOnNewDocument",params);
    if(!pending_) failure_();
  }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;
  std::function<void()> ready_,failure_;std::string desired_,installed_,submitted_,identifier_;
  int pending_=0;bool removing_=false,enabling_=false;
  IMPLEMENT_REFCOUNTING(DocumentScripts);
};
}
