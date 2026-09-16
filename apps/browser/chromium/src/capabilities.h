#pragma once
#include "include/cef_download_handler.h"
#include "include/cef_permission_handler.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <filesystem>
#include <functional>
#include <map>
#include <unistd.h>

namespace qos {
// All state lives on CEF's UI thread. No page can grant its own requests.
class Capabilities final : public CefDownloadHandler, public CefPermissionHandler {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  Capabilities(std::string downloads,Emit emit):downloads_(std::move(downloads)),emit_(std::move(emit)) {}
  static std::string FileName(std::string name) {
    for(auto& c:name) if(c=='\\') c='/';
    name=std::filesystem::path(name).filename().string();
    for(auto& c:name) if(static_cast<unsigned char>(c)<32 || c==127) c='_';
    if(name.empty() || name=="." || name=="..") name="download";
    if(name.size()>180) name="download";
    return name;
  }
  bool OnBeforeDownload(CefRefPtr<CefBrowser>,CefRefPtr<CefDownloadItem> item,const CefString& suggested,CefRefPtr<CefBeforeDownloadCallback> callback) override {
    int id=item->GetId();
    if(downloads_pending_.size()>=32) return true;
    auto name=FileName(suggested);
    downloads_pending_[id]={callback,name};
    auto d=CefDictionaryValue::Create();d->SetInt("id",id);d->SetString("url",item->GetURL());d->SetString("name",name);emit_("download_prompt",d);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<Capabilities> self,int id){self->DownloadDecision(id,false);},CefRefPtr<Capabilities>(this),id),60000);
    return true;
  }
  void DownloadDecision(int id,bool allow) {
    auto it=downloads_pending_.find(id);if(it==downloads_pending_.end()) return;
    auto pending=std::move(it->second);downloads_pending_.erase(it);
    if(!allow) { CancelDownload(id);return; }
    std::error_code ec;std::filesystem::create_directories(downloads_,ec);
    // mkdtemp atomically reserves a private destination. Untrusted suggested
    // names cannot traverse or overwrite an existing file, even on repetition.
    std::string directory=downloads_+"/qos-download-XXXXXX";
    if(ec || !mkdtemp(directory.data())) { DownloadState(id,"failed","Cannot create download directory");return; }
    pending.callback->Continue(directory+"/"+pending.name,false);
  }
  void CancelDownload(int id) {
    downloads_pending_.erase(id);
    auto it=downloads_active_.find(id);
    if(it!=downloads_active_.end()) { auto cb=it->second;downloads_active_.erase(it);cb->Cancel(); }
    else DownloadState(id,"cancelled","");
  }
  void OnDownloadUpdated(CefRefPtr<CefBrowser>,CefRefPtr<CefDownloadItem> item,CefRefPtr<CefDownloadItemCallback> callback) override {
    if(!item->IsValid()) return;
    int id=item->GetId();
    std::string state=item->IsComplete()?"complete":item->IsCanceled()?"cancelled":item->IsInterrupted()?"failed":"progress";
    if(item->IsInProgress()) {
      if(downloads_active_.size()>=32 && !downloads_active_.count(id)) {callback->Cancel();return;}
      downloads_active_[id]=callback;
    } else { downloads_active_.erase(id);downloads_pending_.erase(id); }
    auto d=CefDictionaryValue::Create();d->SetInt("id",id);d->SetString("state",state);d->SetString("path",item->GetFullPath());
    d->SetString("name",FileName(item->GetSuggestedFileName()));d->SetString("url",item->GetURL());
    d->SetDouble("received",item->GetReceivedBytes());d->SetDouble("total",item->GetTotalBytes());
    d->SetString("error",state=="failed"?"Chromium interrupted download ("+std::to_string(item->GetInterruptReason())+")":"");emit_("download",d);
  }
  bool OnShowPermissionPrompt(CefRefPtr<CefBrowser>,uint64_t prompt,const CefString& origin,uint32_t permissions,CefRefPtr<CefPermissionPromptCallback> callback) override {
    if(permissions_.size()>=32) { callback->Continue(CEF_PERMISSION_RESULT_DENY);return true; }
    int id=++sequence_;permissions_[id]={callback,nullptr,permissions,prompt};
    Prompt(id,origin,PermissionNames(permissions,false));return true;
  }
  bool OnRequestMediaAccessPermission(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,const CefString& origin,uint32_t permissions,CefRefPtr<CefMediaAccessCallback> callback) override {
    if(permissions_.size()>=32) {callback->Cancel();return true;}
    int id=++sequence_;permissions_[id]={nullptr,callback,permissions,0};
    Prompt(id,origin,PermissionNames(permissions,true));return true;
  }
  void OnDismissPermissionPrompt(CefRefPtr<CefBrowser>,uint64_t prompt,cef_permission_request_result_t) override {
    for(auto it=permissions_.begin();it!=permissions_.end();++it) if(it->second.prompt && it->second.prompt==prompt) {int id=it->first;permissions_.erase(it);Closed(id);break;}
  }
  void PermissionDecision(int id,bool allow) {
    auto it=permissions_.find(id);if(it==permissions_.end()) return;
    auto p=it->second;permissions_.erase(it);
    if(p.permission) p.permission->Continue(allow?CEF_PERMISSION_RESULT_ACCEPT:CEF_PERMISSION_RESULT_DENY);
    if(p.media) { if(allow) p.media->Continue(p.mask);else p.media->Cancel(); }
    Closed(id);
  }
  void Close() {
    while(!permissions_.empty()) PermissionDecision(permissions_.begin()->first,false);
    while(!downloads_active_.empty()) CancelDownload(downloads_active_.begin()->first);
    downloads_pending_.clear();
  }
 private:
  static std::string PermissionNames(uint32_t mask,bool media) {
    const char* names[]={"augmented reality","camera pan/tilt/zoom","camera","captured surface control","clipboard","top-level storage","disk quota","local fonts","location","hand tracking","identity provider","idle detection","microphone","MIDI","multiple downloads","notifications","keyboard lock","pointer lock","protected media identifier","protocol handler","storage access","virtual reality","web app installation","window management","file system access","local network access","local network","loopback network","sensors"};
    const char* media_names[]={"microphone","camera","desktop audio","screen capture"};
    std::string text;for(unsigned i=0;i<32;++i) if(mask&(1u<<i)) {if(!text.empty()) text+=", ";text+=media?(i<4?media_names[i]:"unknown media permission"):(i<29?names[i]:"unknown permission");}return text;
  }
  void Prompt(int id,const CefString& origin,const std::string& permissions) {
    auto d=CefDictionaryValue::Create();d->SetInt("id",id);d->SetString("origin",origin);d->SetString("permissions",permissions);emit_("permission_prompt",d);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<Capabilities> self,int id){self->PermissionDecision(id,false);},CefRefPtr<Capabilities>(this),id),60000);
  }
  void Closed(int id) {auto d=CefDictionaryValue::Create();d->SetInt("id",id);emit_("permission_closed",d);}
  void DownloadState(int id,const std::string& state,const std::string& error) {
    auto d=CefDictionaryValue::Create();d->SetInt("id",id);d->SetString("state",state);d->SetString("error",error);d->SetString("path","");d->SetString("name","");d->SetString("url","");d->SetDouble("received",0);d->SetDouble("total",0);emit_("download",d);
  }
  struct Permission {CefRefPtr<CefPermissionPromptCallback> permission;CefRefPtr<CefMediaAccessCallback> media;uint32_t mask;uint64_t prompt;};
  struct Download {CefRefPtr<CefBeforeDownloadCallback> callback;std::string name;};
  std::string downloads_;Emit emit_;int sequence_=0;
  std::map<int,Permission> permissions_;
  std::map<int,Download> downloads_pending_;
  std::map<int,CefRefPtr<CefDownloadItemCallback>> downloads_active_;
  IMPLEMENT_REFCOUNTING(Capabilities);
};
}
