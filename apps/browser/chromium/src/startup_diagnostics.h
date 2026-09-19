#pragma once
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <unistd.h>

namespace qos {
// Trace-only snapshots of three existing numeric Chromium counters. This never
// reads Keychain itself, emits raw DevTools payloads, or opens a debug endpoint.
class StartupDiagnostics final : public CefDevToolsMessageObserver {
 public:
  static bool Enabled() {
    const char* value=std::getenv("QOS_WEBVIEW_PROFILE");
    return value && std::strcmp(value,"trace")==0;
  }
  explicit StartupDiagnostics(CefRefPtr<CefBrowser> browser):browser_(browser) {}
  void Attach() {
    registration_=browser_->GetHost()->AddDevToolsMessageObserver(this);
    for(int delay:{5000,20000})
      CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<StartupDiagnostics> self){self->Snapshot();},CefRefPtr<StartupDiagnostics>(this)),delay);
  }
  void Detach() {registration_=nullptr;browser_=nullptr;pending_.clear();}
  void Loaded() {if(!loaded_){loaded_=true;Snapshot();}}
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    // Assignment of the returned request ID may follow a synchronous reply.
    // Never retain unbounded bytes or output a raw response/error string.
    if(size>16384)return;
    std::string copy(static_cast<const char*>(result),size);
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<StartupDiagnostics> self,int id,bool success,std::string copy){self->Result(id,success,copy);},CefRefPtr<StartupDiagnostics>(this),id,success,std::move(copy)));
  }
 private:
  inline static constexpr const char* names[]={"OSCrypt.Mac.FindGenericPasswordResult","OSCrypt.Mac.FindGenericPasswordError","OSCrypt.Mac.SuccessAfterPreviousError"};
  void Snapshot() {
    if(!browser_ || snapshots_++>=3)return;
    for(int index=0;index<3;++index) {
      auto params=CefDictionaryValue::Create();params->SetString("name",names[index]);params->SetBool("delta",false);
      int id=browser_->GetHost()->ExecuteDevToolsMethod(0,"Browser.getHistogram",params);
      if(id)pending_[id]=index;else Print(index,"request-unavailable");
    }
  }
  void Print(int index,const char* outcome,int code=0,int count=0,int sum=0) {
    const auto elapsed=std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now()-started_).count();
    std::fprintf(stderr,"[chromium-startup] pid=%d elapsed_ms=%lld histogram=%s outcome=%s code=%d count=%d sum=%d\n",int(getpid()),static_cast<long long>(elapsed),names[index],outcome,code,count,sum);
  }
  void Result(int id,bool success,const std::string& bytes) {
    auto found=pending_.find(id);if(!browser_ || found==pending_.end())return;
    int index=found->second;pending_.erase(found);
    auto value=CefParseJSON(bytes,JSON_PARSER_RFC);
    auto root=value && value->GetType()==VTYPE_DICTIONARY?value->GetDictionary():nullptr;
    if(!success){Print(index,"unavailable",root && root->GetType("code")==VTYPE_INT?root->GetInt("code"):0);return;}
    auto histogram=root && root->GetType("histogram")==VTYPE_DICTIONARY?root->GetDictionary("histogram"):nullptr;
    if(!histogram || histogram->GetType("name")!=VTYPE_STRING || histogram->GetString("name")!=names[index] || histogram->GetType("count")!=VTYPE_INT || histogram->GetType("sum")!=VTYPE_INT || histogram->GetType("buckets")!=VTYPE_LIST){Print(index,"invalid-shape");return;}
    auto buckets=histogram->GetList("buckets");if(buckets->GetSize()>64){Print(index,"oversized");return;}
    for(size_t i=0;i<buckets->GetSize();++i){auto bucket=buckets->GetDictionary(i);if(!bucket || bucket->GetType("low")!=VTYPE_INT || bucket->GetType("high")!=VTYPE_INT || bucket->GetType("count")!=VTYPE_INT){Print(index,"invalid-bucket");return;}}
    Print(index,"snapshot",0,histogram->GetInt("count"),histogram->GetInt("sum"));
    for(size_t i=0;i<buckets->GetSize();++i){auto bucket=buckets->GetDictionary(i);std::fprintf(stderr,"[chromium-startup-bucket] pid=%d histogram=%s low=%d high=%d count=%d\n",int(getpid()),names[index],bucket->GetInt("low"),bucket->GetInt("high"),bucket->GetInt("count"));}
  }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;
  std::map<int,int> pending_;unsigned snapshots_=0;bool loaded_=false;
  const std::chrono::steady_clock::time_point started_=std::chrono::steady_clock::now();
  IMPLEMENT_REFCOUNTING(StartupDiagnostics);
};
}
