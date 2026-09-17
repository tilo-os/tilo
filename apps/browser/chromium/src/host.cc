#include "host.h"
#include "protocol.h"
#include "view_geometry.h"
#include "key_mapping.h"
#include "request_filter.h"
#include "document_scripts.h"
#include "capabilities.h"
#include "site_info.h"
#include "navigation_history.h"
#include "include/cef_browser.h"
#include "include/cef_client.h"
#include "include/cef_command_line.h"
#include "include/cef_cookie.h"
#include "include/cef_ssl_info.h"
#include "include/cef_parser.h"
#include "include/cef_process_message.h"
#include "include/cef_request_context.h"
#include "include/cef_version.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include "include/wrapper/cef_helpers.h"
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstring>
#include <deque>
#include <fcntl.h>
#include <filesystem>
#include <iostream>
#include <mutex>
#include <map>
#include <optional>
#include <set>
#include <poll.h>
#include <thread>
#include <unistd.h>
#include <signal.h>

namespace {
constexpr int kOffscreenFrameRate = 60;
constexpr size_t kMaxMediaFrames = 64;
// CEF callbacks only enqueue: a slow UI must never block Chromium's UI thread.
class Output {
 public:
  Output():thread_([this]{ Run(); }) {}
  ~Output() { Stop(); }
  void Send(uint8_t kind,std::string data) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto packet=qos::EncodePacket(kind,data);
    if(kind==2 || kind==3) frame_=std::move(packet);
    else if(events_.size()<256) events_.push_back(std::move(packet));
    else failed_=true;
    cv_.notify_one();
  }
  bool Failed() const { return failed_; }
  void Stop() {
    stopping_=true; cv_.notify_one();
    if(thread_.joinable()) thread_.join();
  }
 private:
  void Run() {
    int flags=fcntl(STDOUT_FILENO,F_GETFL,0); fcntl(STDOUT_FILENO,F_SETFL,flags|O_NONBLOCK);
    for(;;) {
      std::string packet;
      { std::unique_lock<std::mutex> lock(mutex_);
        cv_.wait(lock,[&]{ return stopping_ || !events_.empty() || !frame_.empty(); });
        if(!events_.empty()) { packet=std::move(events_.front()); events_.pop_front(); }
        else if(!frame_.empty()) packet.swap(frame_);
        else break;
      }
      size_t offset=0;
      auto deadline=std::chrono::steady_clock::now()+std::chrono::seconds(2);
      while(offset<packet.size()) {
        pollfd fd{STDOUT_FILENO,POLLOUT,0}; int r=poll(&fd,1,100);
        if(r<0 && errno==EINTR) continue;
        if(r>0 && (fd.revents&POLLOUT)) {
          ssize_t n=write(STDOUT_FILENO,packet.data()+offset,packet.size()-offset);
          if(n>0) { offset+=n; continue; }
          if(n<0 && (errno==EAGAIN || errno==EINTR)) continue;
          failed_=true; return;
        }
        if(r<0 || (r>0 && (fd.revents&(POLLHUP|POLLERR|POLLNVAL))) || (stopping_ && std::chrono::steady_clock::now()>deadline)) { failed_=true; return; }
      }
    }
  }
  std::atomic<bool> stopping_{false},failed_{false};
  std::mutex mutex_; std::condition_variable cv_; std::deque<std::string> events_; std::string frame_; std::thread thread_;
};
Output* output=nullptr;
void Event(const std::string& name,CefRefPtr<CefDictionaryValue> fields=nullptr) {
  if(!fields) fields=CefDictionaryValue::Create();
  fields->SetString("event",name); auto value=CefValue::Create(); value->SetDictionary(fields);
  output->Send(1,CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString());
}
void StringEvent(const std::string& name,const std::string& key,const CefString& text) {
  auto d=CefDictionaryValue::Create();d->SetString(key,text);Event(name,d);
}
class DevToolsClient final : public CefClient {
 private: IMPLEMENT_REFCOUNTING(DevToolsClient);
};
double Number(CefRefPtr<CefDictionaryValue> d,const char* key) {
  return d->GetType(key)==VTYPE_DOUBLE ? d->GetDouble(key) : d->GetInt(key);
}
class Client;
class FaviconDownload final : public CefDownloadImageCallback {
 public:
  FaviconDownload(int tab,uint64_t generation,std::string page):tab_(tab),generation_(generation),page_(std::move(page)) {}
  void OnDownloadImageFinished(const CefString&,int,CefRefPtr<CefImage> image) override;
 private:
  int tab_; uint64_t generation_; std::string page_;
  IMPLEMENT_REFCOUNTING(FaviconDownload);
};
std::map<int,CefRefPtr<Client>> clients;
CefRefPtr<CefRequestContext> shared_context;
void CreateTab(int id);
void PublishZoomLevels();
void PublishFilters();
void Dispatch(CefRefPtr<CefDictionaryValue> command);
bool closing=false;
bool clear_on_exit=false;
bool private_mode=false;
std::string downloads_directory;
CefRefPtr<qos::RequestFilter> request_filter;
CefRefPtr<qos::Downloads> downloads;

class Client final : public CefClient, public CefRenderHandler, public CefLifeSpanHandler,
                     public CefDisplayHandler, public CefLoadHandler, public CefFrameHandler, public CefRequestHandler, public CefFindHandler, public CefContextMenuHandler, public CefDialogHandler {
 public:
  explicit Client(int id):id_(id),capabilities_(new qos::Capabilities([this](const std::string& name,CefRefPtr<CefDictionaryValue> fields){Event(name,fields);})) {}
  void Event(const std::string& name,CefRefPtr<CefDictionaryValue> fields=nullptr) {
    if(!fields) fields=CefDictionaryValue::Create();fields->SetInt("tab",id_);::Event(name,fields);
  }
  void StringEvent(const std::string& name,const std::string& key,const CefString& text) {
    auto d=CefDictionaryValue::Create();d->SetString(key,text);Event(name,d);
  }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }
  void OpenTab(const CefString& url,bool background) {
    if(url.length()>8192) return;auto d=CefDictionaryValue::Create();d->SetString("url",url);d->SetBool("background",background);Event("open_tab",d);
  }
  bool OnOpenURLFromTab(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,const CefString& url,WindowOpenDisposition disposition,bool) override {
    if(disposition==CEF_WOD_NEW_BACKGROUND_TAB || disposition==CEF_WOD_NEW_FOREGROUND_TAB || disposition==CEF_WOD_NEW_WINDOW) { OpenTab(url,disposition==CEF_WOD_NEW_BACKGROUND_TAB);return true; }return false;
  }
  CefRefPtr<CefDialogHandler> GetDialogHandler() override { return this; }
  bool OnFileDialog(CefRefPtr<CefBrowser>,FileDialogMode mode,const CefString& title,const CefString&,const std::vector<CefString>&,const std::vector<CefString>& extensions,const std::vector<CefString>&,CefRefPtr<CefFileDialogCallback> callback) override {
#if defined(OS_MAC)
    Event("file_dialog");return ShowHostFileDialog(mode,title,extensions,callback);
#else
    return false;
#endif
  }
  void PublishZoom() { if(browser_) { auto d=CefDictionaryValue::Create();d->SetDouble("level",browser_->GetHost()->GetZoomLevel());Event("zoom",d); } }
  CefRefPtr<CefContextMenuHandler> GetContextMenuHandler() override { return this; }
  void OnBeforeContextMenu(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,CefRefPtr<CefContextMenuParams> params,CefRefPtr<CefMenuModel> model) override {
    if(!params->GetLinkUrl().empty()) { model->AddItem(26501,"Open link in new tab");model->AddItem(26502,"Copy link address"); }
  }
  bool RunContextMenu(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,CefRefPtr<CefContextMenuParams> params,CefRefPtr<CefMenuModel> model,CefRefPtr<CefRunContextMenuCallback> callback) override {
    if(context_callback_) context_callback_->Cancel();
    if(params->GetLinkUrl().ToString().size()>8192) { callback->Cancel();return true; }
    context_callback_=callback;context_id_=context_id_%2147483647+1;context_commands_.clear();
    auto d=CefDictionaryValue::Create();d->SetInt("id",context_id_);d->SetInt("x",params->GetXCoord());d->SetInt("y",params->GetYCoord());d->SetString("link",params->GetLinkUrl());
    auto items=CefListValue::Create();
    for(size_t i=0;i<model->GetCount() && items->GetSize()<24;++i) {
      int command=model->GetCommandIdAt(i);if(command<=0 || !model->IsEnabledAt(i) || model->GetTypeAt(i)==MENUITEMTYPE_SUBMENU) continue;
      if(model->GetLabelAt(i).ToString().size()>4096) continue;
      auto item=CefDictionaryValue::Create();item->SetInt("id",command);item->SetString("label",model->GetLabelAt(i));items->SetDictionary(items->GetSize(),item);context_commands_.insert(command);
    }
    d->SetList("items",items);Event("context_menu",d);return true;
  }
  bool OnContextMenuCommand(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,CefRefPtr<CefContextMenuParams> params,int command,EventFlags) override {
    if(command==26501) { OpenTab(params->GetLinkUrl(),false);return true; }return false;
  }
  void OnContextMenuDismissed(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>) override { context_callback_=nullptr;context_commands_.clear();auto d=CefDictionaryValue::Create();d->SetInt("id",context_id_);Event("context_closed",d); }
  CefRefPtr<CefFindHandler> GetFindHandler() override { return this; }
  void OnFindResult(CefRefPtr<CefBrowser>,int identifier,int count,const CefRect&,int active,bool final_update) override {
    if(final_update && count>0 && active==0 && select_initial_) {
      select_initial_=false;
      CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self){if(self->browser_ && self->finding_) self->browser_->GetHost()->Find(self->find_text_,true,false,true);},CefRefPtr<Client>(this)));
      return;
    }
    auto d=CefDictionaryValue::Create();d->SetInt("id",find_id_);d->SetInt("count",count);d->SetInt("active",active);d->SetBool("final",final_update);Event("find_result",d);
    if(final_update) { finding_=false;if(!pending_find_.empty()) { auto next=pending_find_.front();pending_find_.pop_front();CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self,CefRefPtr<CefDictionaryValue> command){self->Command(command);},CefRefPtr<Client>(this),next));} }
  }
  CefRefPtr<CefDownloadHandler> GetDownloadHandler() override { return downloads; }
  CefRefPtr<CefPermissionHandler> GetPermissionHandler() override { return capabilities_; }
  CefRefPtr<CefRenderHandler> GetRenderHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefFrameHandler> GetFrameHandler() override { return this; }
  void OnFrameCreated(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame) override { SendFilters(frame);SendScripts(frame); }
  void GetViewRect(CefRefPtr<CefBrowser>,CefRect& rect) override { rect=CefRect(0,0,geometry_.logical_width(),geometry_.logical_height()); }
  bool GetScreenInfo(CefRefPtr<CefBrowser>,CefScreenInfo& info) override {
    info.device_scale_factor=geometry_.scale();info.rect=info.available_rect=CefRect(0,0,geometry_.logical_width(),geometry_.logical_height());return true;
  }
  void OnAfterCreated(CefRefPtr<CefBrowser> browser) override {
    browser_=browser;
    request_filter->Begin(browser->GetIdentifier(),"about:blank");
    site_clear_=new qos::SiteClear(browser,[this](int request,const std::string& origin,uint64_t generation,bool success,const std::string& message){SiteResult(request,origin,generation,"clear",success,message);});site_clear_->Attach();
    document_scripts_=new qos::DocumentScripts(browser,[this] { CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self){ self->FlushNavigation(); },CefRefPtr<Client>(this))); },[this] { StringEvent("error","message","Document-start script registration failed");CloseHost(); });
    document_scripts_->Attach();
    navigation_history_=new qos::NavigationHistory(browser,[this](const std::string& name,CefRefPtr<CefDictionaryValue> fields){Event(name,fields);});navigation_history_->Attach();
    auto d=CefDictionaryValue::Create();d->SetInt("frame_rate",kOffscreenFrameRate);d->SetString("cef_version",CEF_VERSION);d->SetString("chromium_version",std::to_string(CHROME_VERSION_MAJOR)+"."+std::to_string(CHROME_VERSION_MINOR)+"."+std::to_string(CHROME_VERSION_BUILD)+"."+std::to_string(CHROME_VERSION_PATCH));Event(id_==1?"ready":"tab_ready",d);
    browser_->GetHost()->WasHidden(!visible_);
    if(closing) browser_->GetHost()->CloseBrowser(true);
  }
  bool DoClose(CefRefPtr<CefBrowser>) override { return false; }
  void OnBeforeClose(CefRefPtr<CefBrowser> browser) override {
    if(browser_ && browser->IsSame(browser_)) { request_filter->Remove(browser->GetIdentifier());site_clear_->Detach();site_clear_=nullptr;capabilities_->Close();navigation_history_->Detach();navigation_history_=nullptr;document_scripts_->Detach();document_scripts_=nullptr;browser_=nullptr;Event("closed");clients.erase(id_);if(closing && clients.empty()) CefQuitMessageLoop(); }
  }
  bool OnBeforeBrowse(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,CefRefPtr<CefRequest> request,bool,bool) override {
    if(frame->IsMain()) { request_filter->Begin(browser->GetIdentifier(),request->GetURL());capabilities_->Close(); }return false;
  }
  bool OnBeforePopup(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame>,int,const CefString& url,const CefString&,WindowOpenDisposition disposition,bool,const CefPopupFeatures&,CefWindowInfo&,CefRefPtr<CefClient>&,CefBrowserSettings&,CefRefPtr<CefDictionaryValue>&,bool*) override {
    OpenTab(url,disposition==CEF_WOD_NEW_BACKGROUND_TAB);return true;
  }
  void OnAddressChange(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,const CefString& url) override { if(frame->IsMain()) { ++favicon_generation_;std::optional<std::pair<std::string,std::pair<uint64_t,bool>>> pending;if(pending_main_media_token_){const auto key=frame->GetIdentifier().ToString();auto found=media_frames_.find(key);if(found!=media_frames_.end() && found->second.first==*pending_main_media_token_)pending=std::make_pair(key,std::make_pair(found->second.first,false));}media_frames_.clear();if(pending)media_frames_.insert(*pending);pending_main_media_token_.reset();media_limit_reported_=false;last_media_playing_.reset();navigation_history_->Invalidate();StringEvent("uri","url",url);PublishMediaState(false); } }
  void OnTitleChange(CefRefPtr<CefBrowser>,const CefString& title) override { StringEvent("title","title",title); }
  void OnFaviconURLChange(CefRefPtr<CefBrowser> browser,const std::vector<CefString>& urls) override {
    if(urls.empty() || urls.front().ToString().size()>8192) return;
    browser->GetHost()->DownloadImage(urls.front(),true,64,false,new FaviconDownload(id_,favicon_generation_,browser->GetMainFrame()->GetURL()));
  }
  void PublishFavicon(uint64_t generation,const std::string& page,CefRefPtr<CefImage> image) {
    if(!browser_ || generation!=favicon_generation_ || browser_->GetMainFrame()->GetURL()!=page || !image || image->IsEmpty()) return;
    int width=0,height=0;auto png=image->GetAsPNG(1.0f,true,width,height);
    if(!png || width<=0 || height<=0 || width>64 || height>64 || png->GetSize()>256*1024) return;
    std::string bytes(png->GetSize(),'\0');png->GetData(bytes.data(),bytes.size(),0);
    auto d=CefDictionaryValue::Create();d->SetString("page_url",page);d->SetInt("width",width);d->SetInt("height",height);d->SetString("png",CefBase64Encode(bytes.data(),bytes.size()));Event("favicon",d);
  }
  void OnLoadingProgressChange(CefRefPtr<CefBrowser>,double progress) override { auto d=CefDictionaryValue::Create();d->SetDouble("progress",progress);Event("progress",d); }
  void OnLoadingStateChange(CefRefPtr<CefBrowser>,bool,bool back,bool forward) override { auto d=CefDictionaryValue::Create();d->SetBool("back",back);d->SetBool("forward",forward);Event("history",d); }
  void OnLoadEnd(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,int status) override {
    if(!frame->IsMain()) return; pending_main_media_token_.reset();PublishZoom();auto d=CefDictionaryValue::Create();d->SetString("url",frame->GetURL());d->SetInt("status",status);Event("loaded",d);SendScripts();
  }
  void OnLoadError(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,ErrorCode code,const CefString& text,const CefString& url) override {
    if(!frame->IsMain() || code==ERR_ABORTED) return; auto d=CefDictionaryValue::Create();d->SetString("url",url);d->SetString("message",text);Event("error",d);
  }
  void OnPopupShow(CefRefPtr<CefBrowser>,bool show) override { popup_visible_=show;if(!show) { popup_.clear(); EmitFrame(); } }
  void OnPopupSize(CefRefPtr<CefBrowser>,const CefRect& rect) override { popup_rect_=rect; }
  void OnPaint(CefRefPtr<CefBrowser> browser,PaintElementType type,const RectList&,const void* buffer,int width,int height) override {
    if(!visible_) return;
    auto size=qos::FrameSize(width,height);if(!size) return;
    if(type==PET_VIEW && !geometry_.PaintMatches(width,height)) {
      if(!stale_paint_recovery_pending_) { stale_paint_recovery_pending_=true;browser->GetHost()->Invalidate(PET_VIEW); }
      return;
    }
    if(type==PET_VIEW) { stale_paint_recovery_pending_=false;pixels_.assign(static_cast<const char*>(buffer),size);pixel_width_=width;pixel_height_=height; }
    else { popup_.assign(static_cast<const char*>(buffer),size);popup_width_=width;popup_height_=height; }
    EmitFrame();
  }
  void EmitFrame() {
    if(!visible_ || pixels_.empty()) return;
    std::string body; qos::AppendU32(body,pixel_width_);qos::AppendU32(body,pixel_height_);body+=pixels_;
    if(popup_visible_ && !popup_.empty()) {
      int x=static_cast<int>(std::round(popup_rect_.x*geometry_.scale())),y=static_cast<int>(std::round(popup_rect_.y*geometry_.scale()));
      for(int row=0;row<popup_height_;++row) {
        int dest_y=y+row;if(dest_y<0 || dest_y>=pixel_height_) continue;
        int start=std::max(0,-x),end=std::min(popup_width_,pixel_width_-x);
        if(end>start) std::memcpy(body.data()+8+(size_t(dest_y)*pixel_width_+x+start)*4,popup_.data()+(size_t(row)*popup_width_+start)*4,size_t(end-start)*4);
      }
    }
    if(id_==1) output->Send(2,std::move(body));
    else {std::string routed;qos::AppendU32(routed,id_);routed+=body;output->Send(3,std::move(routed));}
  }
  bool OnProcessMessageReceived(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,CefProcessId source,CefRefPtr<CefProcessMessage> message) override {
    if(source!=PID_RENDERER) return false;
    if(message->GetName()=="qos.media_context" && frame && browser_) {const auto key=frame->GetIdentifier().ToString();const auto token=static_cast<uint64_t>(message->GetArgumentList()->GetDouble(0));if(frame->IsMain())pending_main_media_token_=token;auto found=media_frames_.find(key);if(found!=media_frames_.end()) found->second={token,false};else if(media_frames_.size()<kMaxMediaFrames) media_frames_.emplace(key,std::make_pair(token,false));if(media_frames_.size()==kMaxMediaFrames && !media_limit_reported_){media_limit_reported_=true;auto d=CefDictionaryValue::Create();d->SetInt("tracked_frames",static_cast<int>(media_frames_.size()));Event("media_frame_limit",d);}return true; }
    if(message->GetName()=="qos.media_context_closed" && frame && browser_) {auto found=media_frames_.find(frame->GetIdentifier().ToString());if(found!=media_frames_.end() && found->second.first==static_cast<uint64_t>(message->GetArgumentList()->GetDouble(0))){const bool was_playing=found->second.second;media_frames_.erase(found);if(was_playing)PublishMediaState();}return true;}
    if(message->GetName()=="qos.media_state" && frame && browser_) { auto found=media_frames_.find(frame->GetIdentifier().ToString());const auto token=static_cast<uint64_t>(message->GetArgumentList()->GetDouble(1));if(found!=media_frames_.end() && found->second.first==token) {found->second.second=message->GetArgumentList()->GetBool(0);PublishMediaState();}return true; }
    if(message->GetName()!="qos.config") return false;
    SendFilters(frame,message->GetArgumentList()->GetDouble(0));SendScripts(frame);return true;
  }
  void PublishMediaState(std::optional<bool> forced=std::nullopt) {
    if(!browser_) return;bool playing=forced.value_or(std::any_of(media_frames_.begin(),media_frames_.end(),[](const auto& entry){return entry.second.second;}));
    if(!forced && last_media_playing_==playing) return;last_media_playing_=playing;
    auto d=CefDictionaryValue::Create();d->SetString("page_url",browser_->GetMainFrame()->GetURL());d->SetInt("document_generation",static_cast<int>(favicon_generation_ & 0x7fffffff));d->SetBool("playing",playing);Event("media_state",d);
  }
  void SendFilters(CefRefPtr<CefFrame> frame,double token=0) {
    if(!frame) return;
    auto message=CefProcessMessage::Create("qos.filters");auto args=message->GetArgumentList();
    auto state=request_filter->Snapshot();args->SetString(0,state->rules);args->SetBool(1,state->enabled);
    auto exceptions=CefListValue::Create();for(const auto& origin:state->exceptions)exceptions->SetString(exceptions->GetSize(),origin);args->SetList(2,exceptions);args->SetString(3,browser_?qos::Origin(browser_->GetMainFrame()->GetURL()):std::string());args->SetString(4,frame->GetURL());args->SetDouble(5,token);
    frame->SendProcessMessage(PID_RENDERER,message);
  }
  void RefreshFilters() {if(!browser_)return;UpdateScriptPolicy();std::vector<CefString> ids;browser_->GetFrameIdentifiers(ids);for(const auto& id:ids)SendFilters(browser_->GetFrameByIdentifier(id));}
  void SiteResult(int request,const std::string& origin,uint64_t generation,const std::string& action,bool success,const std::string& message) {
    auto d=CefDictionaryValue::Create();d->SetInt("request_id",request);d->SetString("origin",origin);d->SetDouble("generation",generation);d->SetString("action",action);d->SetBool("success",success);d->SetString("message",message);Event("site_result",d);
  }
  void SiteCommand(CefRefPtr<CefDictionaryValue> command) {
    const auto origin=command->GetString("origin").ToString(),op=command->GetString("op").ToString();const int request=command->GetInt("request_id");
    auto document=request_filter->Document(browser_->GetIdentifier());const auto actual=qos::Origin(browser_->GetMainFrame()->GetURL());
    const auto action=op=="site_status"?"status":command->GetString("action").ToString();
    if(origin.empty() || origin!=actual || !document || document->origin!=actual || (op=="site_action" && Number(command,"generation")!=document->generation)) {
      SiteResult(request,origin,document?document->generation:0,action,false,"Page changed; reopen site information");return;
    }
    if(op=="site_action") {
      if(action=="clear") {
        if(site_clear_->Busy())SiteResult(request,origin,document->generation,action,false,"A storage clear is already running");
        else site_clear_->Clear(request,origin,document->generation);
        return;
      }
      if(action=="revoke") {
        capabilities_->Close();auto context=browser_->GetHost()->GetRequestContext();for(const auto& permission:qos::kSitePermissions)context->SetContentSetting(origin,origin,permission.type,CEF_CONTENT_SETTING_VALUE_BLOCK);
        SiteResult(request,origin,document->generation,action,true,"Permissions blocked. Reload to end existing media sessions.");
      } else if(action=="enable" || action=="disable") {
        auto origins=request_filter->Snapshot()->exceptions;if(action=="disable")origins.insert(origin);else origins.erase(origin);
        if(origins.size()>1024) {SiteResult(request,origin,document->generation,action,false,"Site exception limit reached");return;}
        request_filter->Exceptions(origins);PublishFilters();SiteResult(request,origin,document->generation,action,true,"Protection preference changed; reload to apply all layers");
      }
      return;
    }
    auto state=request_filter->Snapshot();auto d=CefDictionaryValue::Create();d->SetInt("request_id",request);d->SetString("origin",actual);d->SetDouble("generation",document->generation);d->SetDouble("blocked",std::min<uint64_t>(document->blocked.load(),9007199254740991ULL));d->SetBool("global_enabled",state->enabled);d->SetBool("excepted",state->exceptions.count(actual));d->SetBool("effective",state->enabled && !state->exceptions.count(actual));
    auto entry=browser_->GetHost()->GetVisibleNavigationEntry();auto ssl=entry && qos::Origin(entry->GetURL())==actual?entry->GetSSLStatus():nullptr;
    const bool secure=ssl && ssl->IsSecureConnection() && !CefIsCertStatusError(ssl->GetCertStatus());
    d->SetString("security",actual.starts_with("http:")?"insecure":secure?"secure":"unverified");d->SetInt("certificate_status",ssl?ssl->GetCertStatus():0);d->SetInt("tls_version",ssl?ssl->GetSSLVersion():0);d->SetInt("content_status",ssl?ssl->GetContentStatus():0);
    std::string subject,issuer;double expires=0;
    if(ssl)if(auto certificate=ssl->GetX509Certificate()){subject=certificate->GetSubject()->GetDisplayName();issuer=certificate->GetIssuer()->GetDisplayName();expires=certificate->GetValidExpiry().val;}
    d->SetString("subject",subject.substr(0,512));d->SetString("issuer",issuer.substr(0,512));d->SetDouble("expires",expires);d->SetDictionary("permissions",qos::PermissionStatus(browser_->GetHost()->GetRequestContext(),actual));Event("site_status",d);
  }
  void SendScripts(CefRefPtr<CefFrame> frame=nullptr) {
    if(!frame) { if(!browser_) return;frame=browser_->GetMainFrame(); }
    auto message=CefProcessMessage::Create("qos.scripts");auto args=message->GetArgumentList();
    args->SetString(0,ScriptSource());
    frame->SendProcessMessage(PID_RENDERER,message);
  }
  std::string ScriptSource() {
    auto scripts=scripts_;auto policy=request_filter->Snapshot();if(!blocking_scripts_.empty())scripts.push_back(qos::BlockingScriptSource(blocking_scripts_,policy->enabled,policy->exceptions));return qos::ScriptSource(scripts);
  }
  void UpdateScriptPolicy() {if(document_scripts_){document_scripts_->Configure(ScriptSource());SendScripts();}}
  void FlushNavigation() {
    if(!document_scripts_ || document_scripts_->Busy()) return;
    auto pending=std::move(pending_navigation_);pending_navigation_.clear();
    for(auto& command:pending) Command(command);
  }
  void Command(CefRefPtr<CefDictionaryValue> d) {
    CEF_REQUIRE_UI_THREAD();
    std::string op=d->GetString("op");
    if(op=="shutdown") { CloseHost();return; }
    if(!browser_ || closing) return;
    if(op=="site_status" || op=="site_action") {SiteCommand(d);return;}
    if(op=="site_exceptions") {std::set<std::string> origins;auto list=d->GetList("origins");for(size_t i=0;i<list->GetSize();++i)origins.insert(list->GetString(i));request_filter->Exceptions(origins);PublishFilters();return;}
    if(op=="audio_muted") { browser_->GetHost()->SetAudioMuted(d->GetBool("muted"));auto state=CefDictionaryValue::Create();state->SetBool("muted",browser_->GetHost()->IsAudioMuted());Event("audio_muted",state);return; }
    if((op=="navigate" || op=="back" || op=="forward" || op=="reload" || op=="history_entry") && document_scripts_->Busy()) {
      if(pending_navigation_.size()>=256) { StringEvent("error","message","Document-start navigation queue exhausted");CloseHost();return; }
      pending_navigation_.push_back(d->Copy(false));return;
    }
    auto host=browser_->GetHost();auto frame=browser_->GetMainFrame();
    if(op=="visibility") { visible_=d->GetBool("visible");host->WasHidden(!visible_);host->SetFocus(visible_);if(!visible_ && context_callback_) context_callback_->Cancel();if(visible_) host->Invalidate(PET_VIEW);else {pixels_.clear();popup_.clear();} }
    else if(op=="context_action") {
      if(context_callback_ && d->GetInt("id")==context_id_) {
        int command=d->GetInt("command");auto callback=context_callback_;context_callback_=nullptr;
        if(command && context_commands_.count(command)) callback->Continue(command,EVENTFLAG_NONE);else callback->Cancel();
      }
    }
    else if(op=="try_close_tab") { if(downloads->Busy(id_)) Event("close_blocked");else {host->CloseDevTools();host->CloseBrowser(true);} }
    else if(op=="close_tab") {host->CloseDevTools();host->CloseBrowser(true);}
    else if(op=="navigate") frame->LoadURL(d->GetString("url"));
    else if(op=="navigation_history") navigation_history_->Request(d->GetInt("request_id"),d->GetString("direction") == "forward");
    else if(op=="history_entry") navigation_history_->Navigate(d->GetInt("request_id"),d->GetInt("id"));
    else if(op=="back") browser_->GoBack();
    else if(op=="forward") browser_->GoForward();
    else if(op=="reload") browser_->Reload();
    else if(op=="stop") browser_->StopLoad();
    else if(op=="find") {
      if(finding_) { if(pending_find_.size()<64) pending_find_.push_back(d->Copy(false)); }
      else { finding_=true;find_id_=d->GetInt("id");find_text_=d->GetString("text");select_initial_=!d->GetBool("next");host->Find(d->GetString("text"),d->GetBool("forward"),false,d->GetBool("next")); }
    }
    else if(op=="stop_find") { pending_find_.clear();finding_=false;host->StopFinding(true); }
    else if(op=="zoom") {
      host->SetZoomLevel(Number(d,"level"));
      // Chromium zoom belongs to the origin in the shared request context.
      // Publish every affected endpoint's actual level after propagation.
      CefPostTask(TID_UI,base::BindOnce(&PublishZoomLevels));
      CefPostDelayedTask(TID_UI,base::BindOnce(&PublishZoomLevels),100);
    }
    else if(op=="focus") host->SetFocus(d->GetBool("focused"));
    else if(op=="resize") {
      int w=d->GetInt("width"),h=d->GetInt("height"); double scale=Number(d,"scale_factor");
      if(!std::isfinite(scale)||scale<0.25||scale>8||!qos::FrameSize(w,h)) { CloseHost(); return; }
      // IPC sizes and pointer coordinates are physical pixels; CEF uses DIP.
      const auto effect=geometry_.ResizePhysical(w,h,scale);
      stale_paint_recovery_pending_=false;
      if(effect==qos::ResizeEffect::Repaint) { host->Invalidate(PET_VIEW);return; }
      popup_.clear();pixels_.clear();host->NotifyScreenInfoChanged();host->WasResized();host->Invalidate(PET_VIEW);
    } else if(op=="mouse_move" || op=="mouse_button" || op=="scroll") {
      CefMouseEvent event;event.x=static_cast<int>(Number(d,"x")/geometry_.scale());event.y=static_cast<int>(Number(d,"y")/geometry_.scale());event.modifiers=mouse_modifiers_|key_modifiers_;
      if(op=="mouse_move") host->SendMouseMoveEvent(event,false);
      else if(op=="scroll") host->SendMouseWheelEvent(event,static_cast<int>(Number(d,"dx")/geometry_.scale()),static_cast<int>(Number(d,"dy")/geometry_.scale()));
      else { std::string button=d->GetString("button");auto type=button=="right"?MBT_RIGHT:button=="middle"?MBT_MIDDLE:MBT_LEFT;bool down=d->GetBool("down");
        int flag=type==MBT_RIGHT?EVENTFLAG_RIGHT_MOUSE_BUTTON:type==MBT_MIDDLE?EVENTFLAG_MIDDLE_MOUSE_BUTTON:EVENTFLAG_LEFT_MOUSE_BUTTON;
        if(down) mouse_modifiers_|=flag;else mouse_modifiers_&=~flag; event.modifiers=mouse_modifiers_|key_modifiers_;host->SendMouseClickEvent(event,type,!down,1); }
    } else if(op=="modifiers") key_modifiers_=(d->GetBool("ctrl")?EVENTFLAG_CONTROL_DOWN:0)|(d->GetBool("shift")?EVENTFLAG_SHIFT_DOWN:0)|(d->GetBool("alt")?EVENTFLAG_ALT_DOWN:0)|(d->GetBool("meta")?EVENTFLAG_COMMAND_DOWN:0);
    else if(op=="key") Key(d);
    else if(op=="scripts" || op=="blocking_scripts") {auto& scripts=op=="scripts"?scripts_:blocking_scripts_;scripts.clear();auto list=d->GetList("scripts");if(list) for(size_t i=0;i<list->GetSize();++i) scripts.push_back(list->GetString(i));UpdateScriptPolicy();}
    else if(op=="permission_decision") capabilities_->PermissionDecision(d->GetInt("id"),d->GetBool("allow"));
    else if(op=="clear_on_exit") clear_on_exit=d->GetBool("enabled");
    else if(op=="devtools") { CefWindowInfo window;
      host->ShowDevTools(window,new DevToolsClient,CefBrowserSettings(),CefPoint());
    } else if(op=="clear_data") {
      host->GetRequestContext()->GetCookieManager(nullptr)->DeleteCookies("","",nullptr);
      host->GetRequestContext()->ClearHttpCache(nullptr);host->GetRequestContext()->ClearHttpAuthCredentials(nullptr);
    } else if(op=="filter_rules") {
      const auto rules=d->GetString("rules").ToString();
      const bool enabled=d->GetType("enabled")==VTYPE_NULL ? request_filter->Snapshot()->enabled : d->GetBool("enabled");
      if(!request_filter->Configure(rules,enabled)) { StringEvent("error","message","Native request filter compilation failed; previous rules retained");return; }
      PublishFilters();
    }
    else { StringEvent("error","message","Unknown command: "+op);CloseHost(); }
  }
  void Key(CefRefPtr<CefDictionaryValue> d) {
    CefKeyEvent e;e.type=d->GetBool("down")?KEYEVENT_RAWKEYDOWN:KEYEVENT_KEYUP;
    e.modifiers=(d->GetBool("ctrl")?EVENTFLAG_CONTROL_DOWN:0)|(d->GetBool("shift")?EVENTFLAG_SHIFT_DOWN:0)|(d->GetBool("alt")?EVENTFLAG_ALT_DOWN:0)|(d->GetBool("meta")?EVENTFLAG_COMMAND_DOWN:0);
    key_modifiers_=e.modifiers;
    std::string key=d->GetString("key");
#if defined(OS_MAC)
    // Offscreen CEF has no Cocoa Edit menu to translate Command shortcuts.
    // Execute against the focused frame, preserving native clipboard semantics.
    if(d->GetBool("meta") && !d->GetBool("ctrl") && !d->GetBool("alt") && key.size()==1) {
      char letter=static_cast<char>(std::tolower(static_cast<unsigned char>(key[0])));
      if(letter=='a'||letter=='c'||letter=='x'||letter=='v'||letter=='z') {
        if(d->GetBool("down")) { auto frame=browser_->GetFocusedFrame();if(frame) {
          if(letter=='a') frame->SelectAll();else if(letter=='c') frame->Copy();else if(letter=='x') frame->Cut();else if(letter=='v') frame->Paste();else if(d->GetBool("shift")) frame->Redo();else frame->Undo();
        }}
        return;
      }
    }
#endif
    const auto mapping=qos::MapKey(key);
    e.windows_key_code=mapping.windows;
    e.character=mapping.character;
    e.unmodified_character=mapping.character;
#if defined(OS_MAC)
    e.native_key_code=mapping.mac;
    e.character=mapping.mac_character;
    e.unmodified_character=mapping.mac_character;
#elif defined(OS_LINUX)
    e.native_key_code=mapping.x11;
#endif
    CefString text(key);auto utf16=text.ToString16();
    if(utf16.size()==1) e.character=e.unmodified_character=utf16[0];
    browser_->GetHost()->SendKeyEvent(e);
    if(d->GetBool("down") && (utf16.size()==1 || key=="Enter") && !d->GetBool("ctrl")&&!d->GetBool("meta")) { e.type=KEYEVENT_CHAR;browser_->GetHost()->SendKeyEvent(e); }
  }
  CefRefPtr<CefBrowser> browser_;
 private:
  int id_;bool visible_=true;int key_modifiers_=0;uint64_t favicon_generation_=0;
  std::map<std::string,std::pair<uint64_t,bool>> media_frames_;
  std::optional<uint64_t> pending_main_media_token_;
  std::optional<bool> last_media_playing_;bool media_limit_reported_=false;
  CefRefPtr<qos::NavigationHistory> navigation_history_;
  CefRefPtr<qos::Capabilities> capabilities_;
  CefRefPtr<qos::DocumentScripts> document_scripts_;std::vector<CefRefPtr<CefDictionaryValue>> pending_navigation_;
  CefRefPtr<qos::SiteClear> site_clear_;
  int pixel_width_=0,pixel_height_=0,popup_width_=0,popup_height_=0,mouse_modifiers_=0;
  CefRefPtr<CefRunContextMenuCallback> context_callback_;int context_id_=0;std::set<int> context_commands_;
  bool finding_=false,select_initial_=false;CefString find_text_;int find_id_=0;std::deque<CefRefPtr<CefDictionaryValue>> pending_find_;
  qos::ViewGeometry geometry_;bool stale_paint_recovery_pending_=false;bool popup_visible_=false;CefRect popup_rect_;std::string pixels_,popup_;std::vector<CefString> scripts_,blocking_scripts_;
  IMPLEMENT_REFCOUNTING(Client);
};
void FaviconDownload::OnDownloadImageFinished(const CefString&,int,CefRefPtr<CefImage> image) {
  auto found=clients.find(tab_);if(found!=clients.end()) found->second->PublishFavicon(generation_,page_,image);
}
void PublishFilters() {for(auto& entry:clients)entry.second->RefreshFilters();}
void PublishZoomLevels() { for(auto& entry:clients) entry.second->PublishZoom(); }
void CreateTab(int id) {
  if(closing || id<=0 || clients.count(id) || clients.size()>=32) return;
  auto client=CefRefPtr<Client>(new Client(id));clients.emplace(id,client);
  CefWindowInfo window;window.SetAsWindowless(0);CefBrowserSettings settings;settings.windowless_frame_rate=kOffscreenFrameRate;settings.background_color=CefColorSetARGB(255,255,255,255);
  if(!CefBrowserHost::CreateBrowserSync(window,client,"about:blank",settings,nullptr,shared_context)) {clients.erase(id);CloseHost();}
}
void Dispatch(CefRefPtr<CefDictionaryValue> d) {
  const auto op=d->GetString("op");int id=d->HasKey("tab")?d->GetInt("tab"):1;
  if(op=="shutdown") {CloseHost();return;}
  if(op=="create_tab") {CreateTab(id);return;}
  if(op=="download_decision") { downloads->DownloadDecision(d->GetInt("id"),d->GetBool("allow"));return; }
  if(op=="download_cancel") { downloads->CancelDownload(d->GetInt("id"));return; }
  auto found=clients.find(id);if(found!=clients.end()) found->second->Command(d);
}
class App final : public CefApp,public CefBrowserProcessHandler {
 public:
  CefRefPtr<CefBrowserProcessHandler> GetBrowserProcessHandler() override { return this; }
  void OnBeforeCommandLineProcessing(const CefString& process_type,CefRefPtr<CefCommandLine> command_line) override {
    // The Rust application owns browser onboarding; Chromium's first-run UI
    // cannot be completed in an offscreen embedding and can stall Linux init.
    if(process_type.empty()) command_line->AppendSwitch("no-first-run");
  }
  void OnContextInitialized() override {
    request_filter=new qos::RequestFilter([] { StringEvent("error","message","Native request matcher failed to check a resource"); });
    if(private_mode) {
      CefRequestContextSettings settings;
      // Empty cache_path is CEF's incognito request context. The separate
      // root_cache_path below still gives CEF an owned installation-data root.
      shared_context=CefRequestContext::CreateContext(settings,request_filter);
    } else {
      shared_context=CefRequestContext::CreateContext(CefRequestContext::GetGlobalContext(),request_filter);
    }
    downloads=new qos::Downloads(downloads_directory,[](const std::string& name,CefRefPtr<CefDictionaryValue> fields){ ::Event(name,fields); },[](CefRefPtr<CefBrowser> browser) { for(const auto& entry:clients) if(entry.second->browser_ && browser && entry.second->browser_->IsSame(browser)) return entry.first;return 0; });
    CreateTab(1);if(clients.empty()) CefQuitMessageLoop();
  }
 private: IMPLEMENT_REFCOUNTING(App);
};
bool ValidCommand(CefRefPtr<CefDictionaryValue> d) {
  if(d->HasKey("tab") && (d->GetType("tab")!=VTYPE_INT || d->GetInt("tab")<=0)) return false;
  auto op=d->GetString("op").ToString();
  auto type=[&](const char* key,cef_value_type_t expected){return d->GetType(key)==expected;};
  auto number=[&](const char* key){auto t=d->GetType(key);return t==VTYPE_INT || (t==VTYPE_DOUBLE && std::isfinite(d->GetDouble(key)));};
  if(op=="create_tab" || op=="close_tab" || op=="try_close_tab") return d->HasKey("tab");
  if(op=="visibility") return type("visible",VTYPE_BOOL);
  if(op=="audio_muted") return type("muted",VTYPE_BOOL);
  if(op=="navigation_history" || op=="history_entry") return type("request_id",VTYPE_INT) && d->GetInt("request_id")>0 && (op=="navigation_history" ? (!d->HasKey("direction") || (type("direction",VTYPE_STRING) && (d->GetString("direction")=="back" || d->GetString("direction")=="forward"))) : (type("id",VTYPE_INT) && d->GetInt("id")>0));
  if(op=="navigate") return type("url",VTYPE_STRING) && !d->GetString("url").empty();
  if(op=="clear_on_exit") return type("enabled",VTYPE_BOOL);
  if(op=="permission_decision" || op=="download_decision") return type("id",VTYPE_INT) && d->GetInt("id")>0 && type("allow",VTYPE_BOOL);
  if(op=="context_action") return type("id",VTYPE_INT) && d->GetInt("id")>0 && type("command",VTYPE_INT) && d->GetInt("command")>=0;
  if(op=="find") return type("id",VTYPE_INT) && d->GetInt("id")>0 && type("text",VTYPE_STRING) && d->GetString("text").length()<=4096 && type("forward",VTYPE_BOOL) && type("next",VTYPE_BOOL);
  if(op=="stop_find") return true;
  if(op=="zoom") return number("level") && Number(d,"level")>=-5 && Number(d,"level")<=9;
  if(op=="download_cancel") return type("id",VTYPE_INT) && d->GetInt("id")>0;
  if(op=="modifiers") return type("ctrl",VTYPE_BOOL) && type("shift",VTYPE_BOOL) && type("alt",VTYPE_BOOL) && type("meta",VTYPE_BOOL);
  if(op=="focus") return type("focused",VTYPE_BOOL);
  if(op=="resize") return type("width",VTYPE_INT) && type("height",VTYPE_INT) && number("scale_factor");
  if(op=="mouse_move" || op=="mouse_button" || op=="scroll") {
    if(!number("x") || !number("y") || std::abs(Number(d,"x"))>1000000 || std::abs(Number(d,"y"))>1000000) return false;
    if(op=="scroll") return number("dx") && number("dy") && std::abs(Number(d,"dx"))<=1000000 && std::abs(Number(d,"dy"))<=1000000;
    if(op=="mouse_button") { auto b=d->GetString("button");return type("down",VTYPE_BOOL) && (b=="left" || b=="middle" || b=="right"); }
    return true;
  }
  if(op=="key") return type("key",VTYPE_STRING) && type("down",VTYPE_BOOL);
  if(op=="scripts" || op=="blocking_scripts") {
    if(!type("scripts",VTYPE_LIST)) return false;
    auto list=d->GetList("scripts");for(size_t i=0;i<list->GetSize();++i) if(list->GetType(i)!=VTYPE_STRING) return false;
    return true;
  }
  if(op=="site_exceptions") {if(!type("origins",VTYPE_LIST))return false;auto list=d->GetList("origins");if(list->GetSize()>1024)return false;for(size_t i=0;i<list->GetSize();++i){if(list->GetType(i)!=VTYPE_STRING)return false;const auto origin=list->GetString(i).ToString();if(origin.empty() || qos::Origin(origin)!=origin)return false;}return true;}
  if(op=="site_status" || op=="site_action") {
    if(!type("request_id",VTYPE_INT) || d->GetInt("request_id")<=0 || !type("origin",VTYPE_STRING) || d->GetString("origin").length()>8192)return false;
    if(op=="site_status")return true;
    const auto action=d->GetString("action");return number("generation") && Number(d,"generation")>0 && (action=="clear" || action=="revoke" || action=="enable" || action=="disable");
  }
  if(op=="filter_rules") return type("rules",VTYPE_STRING) && (type("enabled",VTYPE_BOOL) || type("enabled",VTYPE_NULL));
  return op=="back" || op=="forward" || op=="reload" || op=="stop" || op=="clear_data" || op=="devtools" || op=="shutdown";
}
void ReadCommands(std::atomic<bool>& stop) {
  std::string buffer;char chunk[65536];
  while(!stop && !output->Failed()) {
    pollfd fd{STDIN_FILENO,POLLIN,0};int r=poll(&fd,1,100);
    if(r<0 && errno==EINTR) continue;if(r<0) break;if(!r) continue;
    ssize_t n=read(STDIN_FILENO,chunk,sizeof(chunk));if(n<=0) break;buffer.append(chunk,n);
    bool bad=false;
    while(buffer.size()>=4) {
      auto size=qos::U32(buffer.data());if(!size || size>qos::kMaxPacket) {bad=true;break;}
      if(buffer.size()<size_t(size)+4) break;
      if(buffer[4]!=1) {bad=true;break;}
      auto parsed=CefParseJSON(buffer.substr(5,size-1),JSON_PARSER_RFC);
      if(!parsed || parsed->GetType()!=VTYPE_DICTIONARY || parsed->GetDictionary()->GetType("op")!=VTYPE_STRING) {bad=true;break;}
      auto d=parsed->GetDictionary();if(!ValidCommand(d)) {bad=true;break;}CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<CefDictionaryValue> command){Dispatch(command);},d));
      buffer.erase(0,size+4);
    }
    if(bad) { std::cerr<<"Malformed browser IPC packet\n";break; }
  }
  if(!stop) CefPostTask(TID_UI,base::BindOnce(&CloseHost));
}
}
CefRefPtr<CefApp> MakeHostApp() { return new App; }
void CloseHost() {
  closing=true;
  if(downloads) downloads->Close();
  if(clients.empty()) {CefQuitMessageLoop();return;}
  const auto live=clients;for(const auto& [id,client]:live) if(client->browser_) {client->browser_->GetHost()->CloseDevTools();client->browser_->GetHost()->CloseBrowser(true);}
}
int RunHost(int argc,char** argv) {
  signal(SIGPIPE,SIG_IGN);
  std::string profile;
  for(int i=1;i<argc;++i) { std::string arg=argv[i];if(arg.rfind("--profile=",0)==0) profile=arg.substr(10);if(arg.rfind("--downloads=",0)==0) downloads_directory=arg.substr(12);if(arg=="--private") private_mode=true; }
  if(profile.empty() || profile[0]!='/') {std::cerr<<"--profile requires an absolute directory\n";return 2;}
  std::filesystem::create_directories(profile);
  profile=std::filesystem::canonical(profile).string();
  if(downloads_directory.empty()) downloads_directory=profile+"/downloads";
  Output writer;output=&writer;
  CefSettings settings;settings.windowless_rendering_enabled=true;settings.log_severity=LOGSEVERITY_WARNING;
  CefString(&settings.root_cache_path)=profile;
  if(!private_mode) CefString(&settings.cache_path)=profile+"/default";
  CefString(&settings.log_file)=profile+"/cef.log";
#if defined(OS_MAC)
  const auto executable=std::filesystem::canonical(argv[0]);
  const auto contents=executable.parent_path().parent_path();
  // CEF defaults to the outermost app. Our runtime can be nested inside the
  // Rust Browser.app, whose Frameworks directory does not contain CEF directly.
  // Resolve from this executable so standalone and packaged layouts agree.
  CefString(&settings.main_bundle_path)=contents.parent_path().string();
  CefString(&settings.framework_dir_path)=(contents/"Frameworks/Chromium Embedded Framework.framework").string();
  CefString(&settings.browser_subprocess_path)=(contents/"Frameworks/Browser Engine Helper.app/Contents/MacOS/Browser Engine Helper").string();
#endif
  if(!CefInitialize(CefMainArgs(argc,argv),settings,MakeHostApp(),nullptr)) return CefGetExitCode();
  std::atomic<bool> stop{false};std::thread reader([&]{ReadCommands(stop);});
  CefRunMessageLoop();stop=true;reader.join();clients.clear();shared_context=nullptr;request_filter=nullptr;CefShutdown();writer.Stop();output=nullptr;
  if(clear_on_exit) { std::error_code ec;std::filesystem::remove_all(profile+"/default",ec);if(ec) {std::cerr<<"Could not clear Chromium profile: "<<ec.message()<<"\n";return 1;} }
  return writer.Failed()?1:0;
}
