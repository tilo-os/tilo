#pragma once
#include "site_policy.h"
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_request_context.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <functional>
namespace qos {
struct SitePermission { const char* name;cef_content_setting_types_t type; };
inline constexpr SitePermission kSitePermissions[]={
  {"camera",CEF_CONTENT_SETTING_TYPE_MEDIASTREAM_CAMERA},
  {"microphone",CEF_CONTENT_SETTING_TYPE_MEDIASTREAM_MIC},
  {"location",CEF_CONTENT_SETTING_TYPE_GEOLOCATION},
  {"notifications",CEF_CONTENT_SETTING_TYPE_NOTIFICATIONS}};
inline CefRefPtr<CefDictionaryValue> PermissionStatus(CefRefPtr<CefRequestContext> context,const std::string& origin) {
  auto values=CefDictionaryValue::Create();for(const auto& permission:kSitePermissions) {
    auto setting=context->GetContentSetting(origin,origin,permission.type);
    values->SetString(permission.name,setting==CEF_CONTENT_SETTING_VALUE_ALLOW?"allowed":setting==CEF_CONTENT_SETTING_VALUE_BLOCK?"blocked":"ask");
  }return values;
}
// One bounded in-flight clear per browser. Correlated responses never expose
// arbitrary DevTools commands to chrome and never retain a borrowed Client.
class SiteClear final : public CefDevToolsMessageObserver {
 public:
  using Emit=std::function<void(int,const std::string&,uint64_t,bool,const std::string&)>;
  SiteClear(CefRefPtr<CefBrowser> browser,Emit emit):browser_(browser),emit_(std::move(emit)) {}
  void Attach() {registration_=browser_->GetHost()->AddDevToolsMessageObserver(this);}
  void Detach() {registration_=nullptr;browser_=nullptr;emit_=nullptr;pending_=0;}
  bool Busy() const {return pending_!=0;}
  void Clear(int request,const std::string& origin,uint64_t generation) {
    request_=request;origin_=origin;generation_=generation;timed_out_=false;
    auto params=CefDictionaryValue::Create();params->SetString("origin",origin);params->SetString("storageTypes","all");
    pending_=browser_->GetHost()->ExecuteDevToolsMethod(0,"Storage.clearDataForOrigin",params);
    if(!pending_) {Finish(false,"Storage service unavailable");return;}
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<SiteClear> self,int request){if(self->pending_ && self->request_==request && !self->timed_out_){self->timed_out_=true;if(self->emit_)self->emit_(self->request_,self->origin_,self->generation_,false,"Storage clear timed out; it may still be completing");}},CefRefPtr<SiteClear>(this),request),15000);
  }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void*,size_t) override {if(pending_ && id==pending_)Finish(success,success?"Site storage and matching cookies cleared":"Storage service rejected clear");}
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override {if(pending_)Finish(false,"Storage service disconnected");}
 private:
  void Finish(bool success,const std::string& message) {pending_=0;if(emit_)emit_(request_,origin_,generation_,success,message);}
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;Emit emit_;
  int pending_=0,request_=0;bool timed_out_=false;uint64_t generation_=0;std::string origin_;
  IMPLEMENT_REFCOUNTING(SiteClear);
};
}
