#pragma once
#include "include/cef_download_handler.h"
#include "include/cef_permission_handler.h"
#include "include/cef_request_context.h"
#include "site_policy.h"
#include "download_name.h"
#include "download_slots.h"
#include "download_space.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <algorithm>
#include <filesystem>
#include <functional>
#include <deque>
#include <map>
#include <optional>
#include <set>
#include <utility>
#include <unistd.h>

namespace tilo {
// Runtime-owned downloads may outlive their originating browser. CEF download
// item IDs are unique across this browser context; commands use those IDs.
class Downloads final : public CefDownloadHandler {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  Downloads(std::string directory,Emit emit,std::function<int(CefRefPtr<CefBrowser>)> owner,
            std::function<bool(int)> eligible,std::function<bool(int)> can_reserve,
            std::function<void(int,bool)> owner_busy,std::function<void(int)> owner_idle,
            std::function<void()> changed)
      :downloads_(std::move(directory)),emit_(std::move(emit)),owner_(std::move(owner)),
       eligible_(std::move(eligible)),can_reserve_(std::move(can_reserve)),
       owner_busy_(std::move(owner_busy)),owner_idle_(std::move(owner_idle)),changed_(std::move(changed)) {}
  size_t Admitted() const {return slots_.Size();}
  bool Accepting() const {return accepting_;}
  bool OwnerBusy(int owner) const {
    const auto matches=[owner](const auto& entry){return entry.second.owner==owner;};
    return std::any_of(transfers_.begin(),transfers_.end(),matches) ||
           std::any_of(early_.begin(),early_.end(),matches);
  }
  bool Detach(int tab) {
    std::vector<int> pending;
    for(const auto& [id,transfer]:transfers_) if(transfer.owner==tab && !transfer.approved) pending.push_back(id);
    for(int id:pending) CancelDownload(id);
    for(int id:slots_.EarlyForOwner(tab)) CancelEarly(id,"cancelled","");
    return std::any_of(transfers_.begin(),transfers_.end(),[tab](const auto& entry){return entry.second.owner==tab && entry.second.approved;});
  }
  static std::string FileName(std::string name) {return DownloadFileName(std::move(name));}
  // An extension asked for `url` to be downloaded (`chrome.downloads`), on
  // `owner`'s page, under `name` when it is not empty. The download it
  // starts asks the person as any other does; `done` learns its id once it
  // started, or why it did not.
  using Started=std::function<void(int,const std::string&)>;
  void Expect(int owner,std::string url,std::string name,Started done) {
    if(!accepting_) {done(0,"Downloads are turned off");return;}
    if(expected_.size()>=kDownloadLimit) {done(0,"Too many downloads are starting");return;}
    const int key=++expected_sequence_;
    expected_.push_back({key,owner,std::move(url),std::move(name),std::move(done)});
    // A server that never answers starts no download.
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<Downloads> self,int key){self->Unexpect(key,"The download did not start");},CefRefPtr<Downloads>(this),key),60000);
  }
  // The download's file, once it is complete; empty for any other.
  std::string CompletedPath(int id) const {auto found=completed_.find(id);return found==completed_.end()?std::string():found->second;}
  bool Known(int id) const {return transfers_.count(id) || early_.count(id) || terminal_.count(id);}
  bool CanDownload(CefRefPtr<CefBrowser> browser,const CefString& url,const CefString& method) override {
    const int owner=owner_(browser);if(!accepting_ || !owner || !eligible_(owner)) return false;
    provenance_.Record(owner,url.ToString(),method.ToString());
    return true;
  }
  bool OnBeforeDownload(CefRefPtr<CefBrowser> browser,CefRefPtr<CefDownloadItem> item,const CefString& suggested,CefRefPtr<CefBeforeDownloadCallback> callback) override {
    const int id=item->GetId();
    if(terminal_.count(id))return true;
    const int owner=owner_(browser);const auto url=item->GetURL().ToString();
    const bool retryable=provenance_.TakeRetryable(owner,item->GetOriginalUrl().ToString(),url);
    auto early=early_.find(id);const bool staged=early!=early_.end();
    auto expected=TakeExpected(owner,item);
    std::string chosen=staged?early->second.chosen:std::string();
    if(expected && !expected->name.empty()) chosen=FileName(expected->name);
    const auto name=chosen.empty()?FileName(suggested):chosen;
    // Told once the outcome below is known, as Chrome calls back once its
    // download has started.
    struct Tell {std::optional<Expected> expected;int id;~Tell(){if(expected)expected->done(id,"");}} tell{std::move(expected),id};
    const bool source_valid=accepting_ && owner && eligible_(owner);
    const bool owner_capacity=staged || (source_valid && can_reserve_(owner));
    if(!source_valid || !owner_capacity) {
      const std::string error=!source_valid ? "Download source is no longer visible" : "Download owner limit reached (32 retained owners)";
      if(staged) {
        auto transfer=early->second;early_.erase(early);transfers_[id]=std::move(transfer);
        transfers_[id].name=name;transfers_[id].url=url;
        auto active=transfers_[id].active;
        Finish(id,"failed",error);
        if(active)active->Cancel();
      } else TerminalWithoutTransfer(id,name,url,false,"failed",error);
      return true;
    }
    if(!slots_.Admit(id)) {TerminalWithoutTransfer(id,name,url,retryable,"failed","Download limit reached (32 active transfers)");return true;}
    const bool was_busy=OwnerBusy(owner);
    slots_.Promote(id);
    Transfer transfer;if(staged){transfer=early->second;early_.erase(early);}transfer.owner=owner;transfer.before=callback;transfer.name=name;transfer.chosen=chosen;transfer.url=url;transfer.retryable=retryable;
    transfers_[id]=std::move(transfer);
    OwnerStateChanged(owner,was_busy);
    auto d=CefDictionaryValue::Create();d->SetInt("id",id);d->SetString("url",url);d->SetString("name",name);emit_("download_prompt",d);Changed();
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<Downloads> self,int id){self->DownloadDecision(id,false);},CefRefPtr<Downloads>(this),id),60000);
    return true;
  }
  void DownloadDecision(int id,bool allow) {
    auto it=transfers_.find(id);if(it==transfers_.end() || it->second.approved) return;
    if(!allow) { CancelDownload(id);return; }
    std::error_code ec;std::filesystem::create_directories(downloads_,ec);
    // A download must never fill the disk; refuse it before writing a byte.
    if(!ec) {
      const auto& t=it->second;
      const uint64_t remaining=t.total>t.received?static_cast<uint64_t>(t.total-t.received):0;
      if(auto free=tilo::AvailableBytes(downloads_)) {
        auto why=tilo::SpaceRefusal(remaining,*free);
        if(!why.empty()) { Finish(id,"failed",why);return; }
      }
    }
    // mkdtemp atomically reserves a private destination. Untrusted suggested
    // names cannot traverse or overwrite an existing file, even on repetition.
    std::string directory=downloads_+"/tilo-download-XXXXXX";
    if(ec || !mkdtemp(directory.data())) { Finish(id,"failed","Cannot create download directory");return; }
    it->second.approved=true;
    auto callback=it->second.before;it->second.before=nullptr;
    callback->Continue(directory+"/"+it->second.name,false);
    Changed();
  }
  void CancelDownload(int id) {
    auto it=transfers_.find(id);if(it==transfers_.end()) return;
    if(!it->second.approved) {
      auto active=it->second.active;
      Finish(id,"cancelled","");
      if(active)active->Cancel();
      return;
    }
    it->second.cancel_requested=true;
    CefRefPtr<CefDownloadItemCallback> active;
    if(it->second.active && !it->second.cancel_sent) {
      it->second.cancel_sent=true;active=it->second.active;
    }
    if(active)active->Cancel();
    Changed();
  }
  void OnDownloadUpdated(CefRefPtr<CefBrowser> browser,CefRefPtr<CefDownloadItem> item,CefRefPtr<CefDownloadItemCallback> callback) override {
    if(!item->IsValid()) return;
    int id=item->GetId();if(terminal_.count(id)) return;auto it=transfers_.find(id);
    if(it==transfers_.end()) {
      auto staged=early_.find(id);
      if(staged!=early_.end()) {
        Merge(staged->second,item);
        if(item->IsInProgress()){staged->second.active=callback;Changed();return;}
        auto transfer=staged->second;early_.erase(staged);transfers_[id]=std::move(transfer);
        const std::string state=item->IsComplete()?"complete":item->IsCanceled()?"cancelled":"failed";
        Finish(id,state,state=="failed"?InterruptMessage(item->GetInterruptReason()):"");return;
      }
      // An extension's request this download answers learns its id however
      // it goes, as Chrome names a download that failed as it started.
      const int owner=owner_(browser);
      auto expected=TakeExpected(owner,item);
      Stage(owner,id,item,callback,expected?expected->name:std::string());
      if(expected) expected->done(id,"");
      return;
    }
    auto& transfer=it->second;transfer.owner=owner_(browser);Merge(transfer,item);
    if(item->IsInProgress()) {
      transfer.active=callback;
      // As the size becomes known, and while it grows, stop a download the
      // disk can't hold before it fills it.
      if(!transfer.cancel_requested) {
        const uint64_t remaining=transfer.total>transfer.received?static_cast<uint64_t>(transfer.total-transfer.received):0;
        if(auto free=tilo::AvailableBytes(downloads_)) {
          auto why=tilo::SpaceRefusal(remaining,*free);
          if(!why.empty()) {transfer.space_error=why;transfer.cancel_requested=true;}
        }
      }
      if(transfer.cancel_requested) {if(!transfer.cancel_sent){transfer.cancel_sent=true;callback->Cancel();}return;}
      EmitState(id,transfer,"progress","");return;
    }
    if(!transfer.space_error.empty() && !item->IsComplete()) {Finish(id,"failed",transfer.space_error);return;}
    const std::string state=item->IsComplete()?"complete":item->IsCanceled()?"cancelled":"failed";
    Finish(id,state,state=="failed"?InterruptMessage(item->GetInterruptReason()):"");
  }
  // A download Chromium reports before asking where to save it.
  void Stage(int owner,int id,CefRefPtr<CefDownloadItem> item,CefRefPtr<CefDownloadItemCallback> callback,const std::string& chosen) {
    if(!item->IsInProgress()) return;
    if(!accepting_ || !owner || !eligible_(owner) || !can_reserve_(owner)) {
      callback->Cancel();TerminalWithoutTransfer(id,FileName(item->GetSuggestedFileName()),item->GetURL(),false,"failed","Download owner unavailable");return;
    }
    if(!slots_.Stage(id,owner)){callback->Cancel();TerminalWithoutTransfer(id,FileName(item->GetSuggestedFileName()),item->GetURL(),false,"failed","Download limit reached (32 active transfers)");return;}
    const bool was_busy=OwnerBusy(owner);
    Transfer transfer;transfer.owner=owner;transfer.active=callback;
    if(!chosen.empty()) transfer.chosen=FileName(chosen);
    Merge(transfer,item);early_[id]=std::move(transfer);
    OwnerStateChanged(owner,was_busy);
    Changed();
  }
  static std::string InterruptMessage(cef_download_interrupt_reason_t reason) {
    if(reason==CEF_DOWNLOAD_INTERRUPT_REASON_FILE_NO_SPACE) return "Not enough space to finish the download";
    return "Chromium interrupted download ("+std::to_string(reason)+")";
  }
  void Retry(int id,CefRefPtr<CefBrowser> browser) {
    auto found=history_.find(id);if(!accepting_ || !browser || found==history_.end() || !found->second.retryable) return;
    browser->GetHost()->StartDownload(found->second.url);
  }
  void CancelAll() {
    accepting_=false;
    while(!expected_.empty()) Unexpect(expected_.front().key,"Downloads are turned off");
    std::vector<int> staged;for(const auto& entry:early_)staged.push_back(entry.first);
    for(int id:staged)CancelEarly(id,"cancelled","");
    std::vector<int> ids;for(const auto& entry:transfers_)ids.push_back(entry.first);
    for(int id:ids)CancelDownload(id);
    // This event is the acknowledgment barrier consumed by cancel-and-quit.
    Changed();
  }
  void Close() {CancelAll();}
 private:
  // `chosen`: the name an extension asked for, which the download keeps.
  struct Transfer {int owner=0;CefRefPtr<CefBeforeDownloadCallback> before;CefRefPtr<CefDownloadItemCallback> active;std::string name,chosen,url,path;int64_t received=0,total=0;bool retryable=false,approved=false,cancel_requested=false,cancel_sent=false;std::string space_error;};
  struct RetryRecord {std::string url;bool retryable=false;};
  struct Expected {int key=0;int owner=0;std::string url,name;Started done;};
  static constexpr size_t kDownloadLimit=DownloadSlots::kLimit;
  // The extension's request this download answers: the first on its page
  // for the address it asked for, where it went first or where it went in
  // the end (a redirect). A fragment is never fetched.
  static std::string Unfragmented(std::string url) {const auto at=url.find('#');if(at!=std::string::npos)url.resize(at);return url;}
  std::optional<Expected> TakeExpected(int owner,CefRefPtr<CefDownloadItem> item) {
    if(expected_.empty())return std::nullopt;
    const auto original=Unfragmented(item->GetOriginalUrl().ToString()),url=Unfragmented(item->GetURL().ToString());
    for(auto it=expected_.begin();it!=expected_.end();++it) {
      const auto wanted=Unfragmented(it->url);
      if(it->owner!=owner || (wanted!=original && wanted!=url)) continue;
      auto found=std::move(*it);expected_.erase(it);return found;
    }
    return std::nullopt;
  }
  void Unexpect(int key,const std::string& error) {
    for(auto it=expected_.begin();it!=expected_.end();++it) {
      if(it->key!=key) continue;
      auto done=std::move(it->done);expected_.erase(it);done(0,error);return;
    }
  }
  static void Merge(Transfer& transfer,CefRefPtr<CefDownloadItem> item) {
    // The consent callback supplies the destination name. Later item updates
    // can report only Chromium's fallback, even after that filename was saved.
    if(!transfer.before && !transfer.approved)transfer.name=transfer.chosen.empty()?FileName(item->GetSuggestedFileName()):transfer.chosen;
    if(!item->GetURL().empty())transfer.url=item->GetURL();if(!item->GetFullPath().empty())transfer.path=item->GetFullPath();
    if(item->GetReceivedBytes()>0 || !transfer.received)transfer.received=item->GetReceivedBytes();if(item->GetTotalBytes()>0 || !transfer.total)transfer.total=item->GetTotalBytes();
  }
  void EmitState(int id,const Transfer& transfer,const std::string& state,const std::string& error) {
    auto d=CefDictionaryValue::Create();d->SetInt("id",id);d->SetString("state",state);d->SetString("path",transfer.path);d->SetString("name",transfer.name);d->SetString("url",transfer.url);d->SetBool("retryable",transfer.retryable);d->SetDouble("received",transfer.received);d->SetDouble("total",transfer.total);d->SetString("error",error);emit_("download",d);
  }
  void Remember(int id,const Transfer& transfer) {
    history_[id]={transfer.url,transfer.retryable};terminal_.insert(id);terminal_order_.push_back(id);
    while(terminal_order_.size()>100){int old=terminal_order_.front();terminal_order_.pop_front();terminal_.erase(old);history_.erase(old);completed_.erase(old);}
  }
  void OwnerStateChanged(int owner,bool was_busy) {
    const bool busy=OwnerBusy(owner);
    if(busy==was_busy)return;
    owner_busy_(owner,busy);
    if(was_busy && !busy)
      CefPostTask(TID_UI,base::BindOnce([](std::function<void(int)> callback,int id){callback(id);},owner_idle_,owner));
  }
  void Finish(int id,const std::string& state,const std::string& error) {
    if(terminal_.count(id))return;
    auto it=transfers_.find(id);if(it==transfers_.end())return;
    auto transfer=it->second;const bool was_busy=OwnerBusy(transfer.owner);
    transfers_.erase(it);slots_.Settle(id);Remember(id,transfer);
    if(state=="complete" && !transfer.path.empty()) completed_[id]=transfer.path;
    EmitState(id,transfer,state,error);
    OwnerStateChanged(transfer.owner,was_busy);
    Changed();
  }
  void CancelEarly(int id,const std::string& state,const std::string& error) {
    auto found=early_.find(id);if(found==early_.end())return;
    auto transfer=found->second;const bool was_busy=OwnerBusy(transfer.owner);
    early_.erase(found);slots_.Settle(id);Remember(id,transfer);
    auto active=transfer.active;if(active)active->Cancel();
    EmitState(id,transfer,state,error);
    OwnerStateChanged(transfer.owner,was_busy);
    Changed();
  }
  void TerminalWithoutTransfer(int id,const std::string& name,const std::string& url,bool retryable,const std::string& state,const std::string& error) {
    if(terminal_.count(id))return;slots_.Settle(id);Transfer transfer;transfer.name=name;transfer.url=url;transfer.retryable=retryable;Remember(id,transfer);EmitState(id,transfer,state,error);Changed();
  }
  void Changed(){if(changed_)changed_();}
  std::string downloads_;Emit emit_;
  std::function<int(CefRefPtr<CefBrowser>)> owner_;
  std::function<bool(int)> eligible_,can_reserve_;
  std::function<void(int,bool)> owner_busy_;std::function<void(int)> owner_idle_;std::function<void()> changed_;
  bool accepting_=true;
  DownloadSlots slots_;std::map<int,Transfer> transfers_,early_;std::map<int,RetryRecord> history_;std::set<int> terminal_;std::deque<int> terminal_order_;
  DownloadProvenance provenance_;
  std::deque<Expected> expected_;int expected_sequence_=0;std::map<int,std::string> completed_;
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
  // `dismissed`: set aside unanswered (Escape, a click elsewhere, no answer
  // in time), which Chromium does not remember as a block, so the site may
  // ask again. A refusal is remembered, for the camera and microphone as for
  // any other permission, until the person has the site ask again.
  void PermissionDecision(int id,bool allow,bool dismissed=false) {
    Settle(id,allow?CEF_PERMISSION_RESULT_ACCEPT:dismissed?CEF_PERMISSION_RESULT_DISMISS:CEF_PERMISSION_RESULT_DENY);
  }
  // The questions no one answered are withdrawn: the page moved on, its tab
  // closed, or its permissions were blocked. That is no refusal, so nothing
  // is remembered for the site.
  void Close() {
    while(!permissions_.empty()) Settle(permissions_.begin()->first,CEF_PERMISSION_RESULT_IGNORE);
  }
 private:
  struct MediaSetting {uint32_t mask;cef_content_setting_types_t type;};
  inline static constexpr MediaSetting kMediaSettings[]={
    {CEF_MEDIA_PERMISSION_DEVICE_AUDIO_CAPTURE,CEF_CONTENT_SETTING_TYPE_MEDIASTREAM_MIC},
    {CEF_MEDIA_PERMISSION_DEVICE_VIDEO_CAPTURE,CEF_CONTENT_SETTING_TYPE_MEDIASTREAM_CAMERA}};
  void Settle(int id,cef_permission_request_result_t result) {
    auto it=permissions_.find(id);if(it==permissions_.end()) return;
    auto p=it->second;permissions_.erase(it);
    if(p.permission) p.permission->Continue(result);
    if(p.media) {
      // A different tab may revoke the origin while this prompt is pending.
      if(result==CEF_PERMISSION_RESULT_ACCEPT && !MediaBlocked(p.context,p.origin,p.mask)) {
        SetMedia(p,CEF_CONTENT_SETTING_VALUE_ALLOW);
        p.media->Continue(p.mask);
      } else {
        if(result==CEF_PERMISSION_RESULT_DENY) SetMedia(p,CEF_CONTENT_SETTING_VALUE_BLOCK);
        p.media->Cancel();
      }
    }
    Closed(id);
  }
  struct Permission {CefRefPtr<CefPermissionPromptCallback> permission;CefRefPtr<CefMediaAccessCallback> media;uint32_t mask;uint64_t prompt;CefRefPtr<CefRequestContext> context;std::string origin;};
  static void SetMedia(const Permission& p,cef_content_setting_values_t value) {
    for(const auto& device:kMediaSettings) if(p.mask & device.mask)
      p.context->SetContentSetting(p.origin,p.origin,device.type,value);
  }
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
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<Capabilities> self,int id){self->PermissionDecision(id,false,true);},CefRefPtr<Capabilities>(this),id),60000);
  }
  void Closed(int id) {auto d=CefDictionaryValue::Create();d->SetInt("id",id);emit_("permission_closed",d);}
  Emit emit_;int sequence_=0;
  std::map<int,Permission> permissions_;
  IMPLEMENT_REFCOUNTING(Capabilities);
};
}
