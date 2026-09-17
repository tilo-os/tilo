#pragma once
#include "include/cef_download_handler.h"
#include "include/cef_permission_handler.h"
#include "include/cef_request_context.h"
#include "site_policy.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <filesystem>
#include <functional>
#include <map>
#include <set>
#include <unistd.h>

namespace qos {
// Runtime-owned downloads may outlive their originating browser. CEF download
// item IDs are unique across this browser context; commands use those IDs.
class Downloads final : public CefDownloadHandler {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  Downloads(std::string directory,Emit emit,std::function<int(CefRefPtr<CefBrowser>)> owner):downloads_(std::move(directory)),emit_(std::move(emit)),owner_(std::move(owner)) {}
  bool Busy(int tab) {
    std::vector<int> pending;for(const auto& entry:owners_) if(entry.second==tab && !approved_.count(entry.first)) pending.push_back(entry.first);
    for(int id:pending) CancelDownload(id);
    for(const auto& entry:owners_) if(entry.second==tab && approved_.count(entry.first)) return true;
    return false;
  }
  static std::string FileName(std::string name) {
    for(auto& c:name) if(c=='\\') c='/';
    name=std::filesystem::path(name).filename().string();
    for(auto& c:name) if(static_cast<unsigned char>(c)<32 || c==127) c='_';
    if(name.empty() || name=="." || name=="..") name="download";
    if(name.size()>180) name="download";
    return name;
  }
  bool OnBeforeDownload(CefRefPtr<CefBrowser> browser,CefRefPtr<CefDownloadItem> item,const CefString& suggested,CefRefPtr<CefBeforeDownloadCallback> callback) override {
    int id=item->GetId();
    if(downloads_pending_.size()>=32) return true;
    owners_[id]=owner_(browser);
    auto name=FileName(suggested);
    downloads_pending_[id]={callback,name};
    auto d=CefDictionaryValue::Create();d->SetInt("id",id);d->SetString("url",item->GetURL());d->SetString("name",name);emit_("download_prompt",d);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<Downloads> self,int id){self->DownloadDecision(id,false);},CefRefPtr<Downloads>(this),id),60000);
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
    approved_.insert(id);pending.callback->Continue(directory+"/"+pending.name,false);
  }
  void CancelDownload(int id) {
    downloads_pending_.erase(id);
    auto it=downloads_active_.find(id);
    if(it!=downloads_active_.end()) { auto cb=it->second;downloads_active_.erase(it);cb->Cancel(); }
    DownloadState(id,"cancelled","");
  }
  void OnDownloadUpdated(CefRefPtr<CefBrowser> browser,CefRefPtr<CefDownloadItem> item,CefRefPtr<CefDownloadItemCallback> callback) override {
    if(!item->IsValid()) return;
    int id=item->GetId();
    std::string state=item->IsComplete()?"complete":item->IsCanceled()?"cancelled":item->IsInterrupted()?"failed":"progress";
    if(item->IsInProgress()) {
      if(downloads_active_.size()>=32 && !downloads_active_.count(id)) {callback->Cancel();return;}
      downloads_active_[id]=callback;owners_[id]=owner_(browser);
    } else { downloads_active_.erase(id);downloads_pending_.erase(id);owners_.erase(id);approved_.erase(id); }
    auto d=CefDictionaryValue::Create();d->SetInt("id",id);d->SetString("state",state);d->SetString("path",item->GetFullPath());
    d->SetString("name",FileName(item->GetSuggestedFileName()));d->SetString("url",item->GetURL());
    d->SetDouble("received",item->GetReceivedBytes());d->SetDouble("total",item->GetTotalBytes());
    d->SetString("error",state=="failed"?"Chromium interrupted download ("+std::to_string(item->GetInterruptReason())+")":"");emit_("download",d);
  }
  void Close() {
    while(!downloads_active_.empty()) CancelDownload(downloads_active_.begin()->first);
    downloads_pending_.clear();
  }
 private:
  void DownloadState(int id,const std::string& state,const std::string& error) {
    owners_.erase(id);approved_.erase(id);
    auto d=CefDictionaryValue::Create();d->SetInt("id",id);d->SetString("state",state);d->SetString("error",error);d->SetString("path","");d->SetString("name","");d->SetString("url","");d->SetDouble("received",0);d->SetDouble("total",0);emit_("download",d);
  }
  struct Download {CefRefPtr<CefBeforeDownloadCallback> callback;std::string name;};
  std::string downloads_;Emit emit_;
  std::function<int(CefRefPtr<CefBrowser>)> owner_;
  std::map<int,int> owners_;
  std::set<int> approved_;
  std::map<int,Download> downloads_pending_;
  std::map<int,CefRefPtr<CefDownloadItemCallback>> downloads_active_;
  IMPLEMENT_REFCOUNTING(Downloads);
};
// Pending prompts belong to one live page. Device grants and revocation use
// the same exact-origin content settings reported by site information.
class Capabilities final : public CefPermissionHandler {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  explicit Capabilities(Emit emit):emit_(std::move(emit)) {}
  bool OnShowPermissionPrompt(CefRefPtr<CefBrowser>,uint64_t prompt,const CefString& origin,uint32_t permissions,CefRefPtr<CefPermissionPromptCallback> callback) override {
    if(permissions_.size()>=32) { callback->Continue(CEF_PERMISSION_RESULT_DENY);return true; }
    int id=++sequence_;permissions_[id]={callback,nullptr,permissions,prompt};
    Prompt(id,origin,PermissionNames(permissions,false));return true;
  }
  bool OnRequestMediaAccessPermission(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame>,const CefString& requesting_origin,uint32_t permissions,CefRefPtr<CefMediaAccessCallback> callback) override {
    const auto origin=Origin(requesting_origin);auto context=browser->GetHost()->GetRequestContext();
    if(origin.empty() || MediaBlocked(context,origin,permissions)) {callback->Cancel();return true;}
    constexpr auto devices=CEF_MEDIA_PERMISSION_DEVICE_AUDIO_CAPTURE|CEF_MEDIA_PERMISSION_DEVICE_VIDEO_CAPTURE;
    bool granted=permissions && !(permissions & ~devices);
    for(const auto& device:kMediaSettings) if(permissions & device.mask)
      granted=granted && context->GetContentSetting(origin,origin,device.type)==CEF_CONTENT_SETTING_VALUE_ALLOW;
    if(granted) {callback->Continue(permissions);return true;}
    if(permissions_.size()>=32) {callback->Cancel();return true;}
    int id=++sequence_;permissions_[id]={nullptr,callback,permissions,0,context,origin};
    Prompt(id,origin,PermissionNames(permissions,true));return true;
  }
  void OnDismissPermissionPrompt(CefRefPtr<CefBrowser>,uint64_t prompt,cef_permission_request_result_t) override {
    for(auto it=permissions_.begin();it!=permissions_.end();++it) if(it->second.prompt && it->second.prompt==prompt) {int id=it->first;permissions_.erase(it);Closed(id);break;}
  }
  void PermissionDecision(int id,bool allow) {
    auto it=permissions_.find(id);if(it==permissions_.end()) return;
    auto p=it->second;permissions_.erase(it);
    if(p.permission) p.permission->Continue(allow?CEF_PERMISSION_RESULT_ACCEPT:CEF_PERMISSION_RESULT_DENY);
    if(p.media) {
      // A different tab may revoke the origin while this prompt is pending.
      allow=allow && !MediaBlocked(p.context,p.origin,p.mask);
      if(allow) {
        for(const auto& device:kMediaSettings) if(p.mask & device.mask)
          p.context->SetContentSetting(p.origin,p.origin,device.type,CEF_CONTENT_SETTING_VALUE_ALLOW);
        p.media->Continue(p.mask);
      } else p.media->Cancel();
    }
    Closed(id);
  }
  void Close() {
    while(!permissions_.empty()) PermissionDecision(permissions_.begin()->first,false);
  }
 private:
  struct MediaSetting {uint32_t mask;cef_content_setting_types_t type;};
  inline static constexpr MediaSetting kMediaSettings[]={
    {CEF_MEDIA_PERMISSION_DEVICE_AUDIO_CAPTURE,CEF_CONTENT_SETTING_TYPE_MEDIASTREAM_MIC},
    {CEF_MEDIA_PERMISSION_DEVICE_VIDEO_CAPTURE,CEF_CONTENT_SETTING_TYPE_MEDIASTREAM_CAMERA}};
  static bool MediaBlocked(CefRefPtr<CefRequestContext> context,const std::string& origin,uint32_t mask) {
    for(const auto& device:kMediaSettings) if((mask & device.mask) && context->GetContentSetting(origin,origin,device.type)==CEF_CONTENT_SETTING_VALUE_BLOCK) return true;
    return false;
  }
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
  struct Permission {CefRefPtr<CefPermissionPromptCallback> permission;CefRefPtr<CefMediaAccessCallback> media;uint32_t mask;uint64_t prompt;CefRefPtr<CefRequestContext> context;std::string origin;};
  Emit emit_;int sequence_=0;
  std::map<int,Permission> permissions_;
  IMPLEMENT_REFCOUNTING(Capabilities);
};
}
