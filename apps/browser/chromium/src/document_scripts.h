#pragma once
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include <functional>
#include <set>
#include <cstdlib>
#include <cstring>
#include <iostream>

namespace tilo {
inline std::string ScriptSource(const std::vector<CefString>& scripts) {
  auto list=CefListValue::Create();for(size_t i=0;i<scripts.size();++i) list->SetString(i,scripts[i]);
  auto value=CefValue::Create();value->SetList(list);
  const auto json=CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString();
  // Registration is browser-wide, but actual scripts are restricted to direct
  // HTTP(S) documents. The same source is used for live updates. Execute trusted
  // bodies directly: page eval() is subject to CSP on already loaded documents.
  auto signature=CefValue::Create();signature->SetString(json);
  const auto quoted=CefWriteJSON(signature,JSON_WRITER_DEFAULT).ToString();
  const std::string marker="window[Symbol.for('tilo.documentScripts')]";
  std::string source="if((location.protocol==='http:'||location.protocol==='https:')&&"+marker+"!=="+quoted+"){"+marker+"="+quoted+";\n";
  for(const auto& script:scripts) source+="try {\n"+script.ToString()+"\n} catch(tiloScriptError) { console.error('Tilo document script failed',tiloScriptError); }\n";
  return source+"}";
}
// Supplemental blocking scripts have the same top-level-origin policy as the
// native matcher. ancestorOrigins is Chromium-provided even in cross-process
// subframes; no cross-origin DOM read or page eval is required.
inline std::string BlockingScriptSource(const std::vector<CefString>& scripts,bool enabled,const std::set<std::string>& exceptions) {
  auto origins=CefListValue::Create();for(const auto& origin:exceptions)origins->SetString(origins->GetSize(),origin);auto value=CefValue::Create();value->SetList(origins);
  std::string source="(()=>{const a=location.ancestorOrigins;const o=a&&a.length?a[a.length-1]:location.origin;if("+std::string(enabled?"true":"false")+"&&!"+CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString()+".includes(o)){";
  for(const auto& script:scripts)source+="try{\n"+script.ToString()+"\n}catch(tiloBlockingScriptError){}\n";
  return source+"}})()";
}
class DocumentScripts final : public CefDevToolsMessageObserver {
 public:
  DocumentScripts(CefRefPtr<CefBrowser> browser,std::function<void()> ready,std::function<void()> failure,std::string world={})
    :browser_(browser),ready_(std::move(ready)),failure_(std::move(failure)),world_(std::move(world)) {}
  void Attach() {
    registration_=browser_->GetHost()->AddDevToolsMessageObserver(this);
    enabling_=true;Trace("enable-submit",0);pending_=browser_->GetHost()->ExecuteDevToolsMethod(0,"Page.enable",nullptr);Trace("enable-return",pending_);
    if(!pending_) failure_();
  }
  void Detach() { registration_=nullptr;browser_=nullptr; }
  bool Busy() const { return pending_!=0 || desired_!=installed_; }
  void Configure(const std::string& source) { desired_=source;if(!pending_) Start(); }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    Trace("result",id);
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
  void Trace(const char* stage,int id) const {
    const char* trace=std::getenv("TILO_WEBVIEW_PROFILE");
    if(trace && std::strcmp(trace,"trace")==0)
      std::cerr<<"tilo-script-registration "<<stage<<" id="<<id<<" pending="<<pending_<<" enabling="<<enabling_<<"\n";
  }
  void Start() {
    if(!browser_ || desired_==installed_) { if(browser_) ready_();return; }
    auto params=CefDictionaryValue::Create();
    removing_=!identifier_.empty();
    if(removing_) params->SetString("identifier",identifier_);
    else { submitted_=desired_;params->SetString("source",submitted_);if(!world_.empty())params->SetString("worldName",world_); }
    pending_=browser_->GetHost()->ExecuteDevToolsMethod(0,removing_?"Page.removeScriptToEvaluateOnNewDocument":"Page.addScriptToEvaluateOnNewDocument",params);
    if(!pending_) failure_();
  }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;
  std::function<void()> ready_,failure_;std::string desired_,installed_,submitted_,identifier_,world_;
  int pending_=0;bool removing_=false,enabling_=false;
  IMPLEMENT_REFCOUNTING(DocumentScripts);
};
}
