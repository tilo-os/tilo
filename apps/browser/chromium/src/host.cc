#include "host.h"
#include "protocol.h"
#include "view_geometry.h"
#include "key_mapping.h"
#include "request_filter.h"
#include "document_scripts.h"
#include "startup_diagnostics.h"
#include "capabilities.h"
#include "site_info.h"
#include "navigation_history.h"
#include "page_automation.h"
#include "agent_log.h"
#include "paint_snapshot.h"
#include "fullscreen_state.h"
#include "audio_pcm.h"
#include "credential_document.h"
#include "credential_consent.h"
#include "credential_commands.h"
#include "devtools_bridge.h"
#if defined(OS_MAC)
#include "credentials_mac.h"
#endif
#include "include/cef_audio_handler.h"
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
#include <cstdlib>
#include <cstdio>
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
constexpr size_t kMaxAudioPackets = 16;
// Bounded, opt-in startup geometry evidence; never includes page content.
void TraceGeometry(const char* stage,int tab,int width=0,int height=0,double scale=0) {
  static const bool enabled=[] {const char* value=std::getenv("TILO_WEBVIEW_PROFILE");return value && std::strcmp(value,"trace")==0;}();
  static std::atomic<unsigned> count{0};
  if(enabled && count.fetch_add(1)<128) std::fprintf(stderr,"[chromium-geometry] %s tab=%d %dx%d scale=%g\n",stage,tab,width,height,scale);
}
// CEF callbacks only enqueue: a slow UI must never block Chromium's UI thread.
class Output {
 public:
  Output():thread_([this]{ Run(); }) {}
  ~Output() { Stop(); }
  // Page audio is real time: its own lane, ordered with itself (start, packets,
  // stop) but never allowed to fail the host or hold up its events. Past
  // kMaxAudioPackets queued the oldest packet goes, so a slow app hears a gap
  // instead of an ever-growing delay.
  void SendAudio(std::string data,bool packet) {
    std::lock_guard<std::mutex> lock(mutex_);
    audio_.emplace_back(packet,tilo::EncodePacket(1,data));
    if(audio_.size()>kMaxAudioPackets) {
      for(auto it=audio_.begin();it!=audio_.end();++it) if(it->first) { audio_.erase(it);break; }
    }
    cv_.notify_one();
  }
  void Send(uint8_t kind,std::string data) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto packet=tilo::EncodePacket(kind,data);
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
        cv_.wait(lock,[&]{ return stopping_ || !events_.empty() || !audio_.empty() || !frame_.empty(); });
        if(!events_.empty()) { packet=std::move(events_.front()); events_.pop_front(); }
        else if(!audio_.empty()) { packet=std::move(audio_.front().second); audio_.pop_front(); }
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
  std::mutex mutex_; std::condition_variable cv_; std::deque<std::string> events_; std::deque<std::pair<bool,std::string>> audio_; std::string frame_; std::thread thread_;
};
Output* output=nullptr;
// Page audio goes to the app (`--tilo-audio-bridge`) because its sandbox has no audio device; without it Chromium plays on its own.
bool audio_bridge=false;
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
void PublishDownloadRuntime();
void DownloadOwnerIdle(int id);
void Dispatch(CefRefPtr<CefDictionaryValue> command);
void DevToolsCall(CefRefPtr<CefDictionaryValue> d,int tab);
bool closing=false;
bool clear_on_exit=false;
bool private_mode=false;
// Extension management over Chromium's DevTools pipe (see devtools_bridge.h).
// The host's ends of the pipe; -1 when the pipe is not available.
int devtools_write=-1,devtools_read=-1;
tilo::DevToolsPending devtools_pending;
std::string downloads_directory;
std::shared_ptr<tilo::CredentialWorker> credential_worker;
struct CredentialDeliveryState {std::mutex mutex;bool active=false;};
// A blocked worker retains this plain state through shutdown, without touching
// destroyed globals or retaining CEF objects after CefShutdown.
const auto credential_delivery=std::make_shared<CredentialDeliveryState>();
CefRefPtr<tilo::RequestFilter> request_filter;
CefRefPtr<tilo::Downloads> downloads;
tilo::DownloadOwnerReservations download_owners;

class Client final : public CefClient, public CefRenderHandler, public CefLifeSpanHandler,
                     public CefDisplayHandler, public CefLoadHandler, public CefFrameHandler, public CefRequestHandler, public CefFindHandler, public CefContextMenuHandler, public CefDialogHandler, public CefAudioHandler {
 public:
  explicit Client(int id):id_(id),credential_document_(private_mode),capabilities_(new tilo::Capabilities([this](const std::string& name,CefRefPtr<CefDictionaryValue> fields){Event(name,fields);})) {}
  void Event(const std::string& name,CefRefPtr<CefDictionaryValue> fields=nullptr) {
    if(transfer_owner_ && name!="tab_detached" && name!="closed") return;
    if(!fields) fields=CefDictionaryValue::Create();fields->SetInt("tab",id_);::Event(name,fields);
  }
  void StringEvent(const std::string& name,const std::string& key,const CefString& text) {
    auto d=CefDictionaryValue::Create();d->SetString(key,text);Event(name,d);
  }
  // Called on CEF's audio threads: only enqueues, like every callback here.
  void AudioEvent(const std::string& name,CefRefPtr<CefDictionaryValue> fields,bool packet) {
    if(transfer_owner_) return;
    if(!fields) fields=CefDictionaryValue::Create();
    fields->SetInt("tab",id_);fields->SetString("event",name);
    auto value=CefValue::Create();value->SetDictionary(fields);
    output->SendAudio(CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString(),packet);
  }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }
  void OpenTab(const CefString& url,bool background) {
    if(url.length()>8192) return;auto d=CefDictionaryValue::Create();d->SetString("url",url);d->SetBool("background",background);Event("open_tab",d);
  }
  bool OnOpenURLFromTab(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,const CefString& url,cef_window_open_disposition_t disposition,bool) override {
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
  CefRefPtr<CefAudioHandler> GetAudioHandler() override { return audio_bridge ? this : nullptr; }
  // Stereo at 48 kHz is what the app's output service plays: Chromium mixes
  // and resamples to it before handing the stream over.
  bool GetAudioParameters(CefRefPtr<CefBrowser>,CefAudioParameters& params) override {
    params.channel_layout=CEF_CHANNEL_LAYOUT_STEREO;params.sample_rate=48000;params.frames_per_buffer=480;return true;
  }
  void OnAudioStreamStarted(CefRefPtr<CefBrowser>,const CefAudioParameters& params,int channels) override {
    audio_channels_=channels;
    auto d=CefDictionaryValue::Create();d->SetInt("rate",params.sample_rate);d->SetInt("channels",channels);AudioEvent("audio_start",d,false);
  }
  void OnAudioStreamPacket(CefRefPtr<CefBrowser>,const float** data,int frames,int64_t) override {
    std::string pcm;
    if(!tilo::InterleavePcm16(data,audio_channels_,frames,pcm)) return;
    auto d=CefDictionaryValue::Create();d->SetString("pcm",CefBase64Encode(pcm.data(),pcm.size()));AudioEvent("audio",d,true);
  }
  void OnAudioStreamStopped(CefRefPtr<CefBrowser>) override { AudioEvent("audio_stop",nullptr,false); }
  void OnAudioStreamError(CefRefPtr<CefBrowser>,const CefString&) override { AudioEvent("audio_stop",nullptr,false); }
  CefRefPtr<CefDownloadHandler> GetDownloadHandler() override { return downloads; }
  CefRefPtr<CefPermissionHandler> GetPermissionHandler() override { return capabilities_; }
  CefRefPtr<CefRenderHandler> GetRenderHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefFrameHandler> GetFrameHandler() override { return this; }
  void OnFullscreenModeChange(CefRefPtr<CefBrowser>,bool fullscreen) override {
    if(!fullscreen_.Set(fullscreen)) return;
    auto d=CefDictionaryValue::Create();d->SetBool("fullscreen",fullscreen);Event("fullscreen",d);
  }
  void OnRenderProcessTerminated(CefRefPtr<CefBrowser>,TerminationStatus,int,const CefString&) override {
    InvalidateCredentials();
    fullscreen_.ResetForRendererTermination();
    // Publish the lifecycle invalidation before terminal outcomes: a consumer
    // may admit recovery immediately after observing its owned completion.
    Event("renderer_terminated");
    if(navigation_owner_)NavigationOutcome(*navigation_owner_,browser_->GetMainFrame()->GetURL(),"Renderer terminated");
    for(const auto& command:pending_navigation_)if(command->GetString("op")=="scoped_navigation")NavigationOutcome(command->GetInt("request_id"),browser_->GetMainFrame()->GetURL(),"Renderer terminated");
    pending_navigation_.clear();navigation_owner_.reset();navigation_dispatched_=false;navigation_started_=false;navigation_busy_=false;navigation_aborting_=false;bootstrap_settled_=true;
  }
  void OnFrameCreated(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame) override { SendFilters(frame);SendScripts(frame); }
  void GetViewRect(CefRefPtr<CefBrowser>,CefRect& rect) override { rect=CefRect(0,0,geometry_.logical_width(),geometry_.logical_height()); }
  bool GetScreenInfo(CefRefPtr<CefBrowser>,CefScreenInfo& info) override {
    info.device_scale_factor=geometry_.scale();info.rect=info.available_rect=CefRect(0,0,geometry_.logical_width(),geometry_.logical_height());return true;
  }
  void OnAfterCreated(CefRefPtr<CefBrowser> browser) override {
    browser_=browser;
    if(id_==1 && tilo::StartupDiagnostics::Enabled()){startup_diagnostics_=new tilo::StartupDiagnostics(browser);startup_diagnostics_->Attach();}
    request_filter->Begin(browser->GetIdentifier(),"about:blank");
    site_clear_=new tilo::SiteClear(browser,[this](int request,const std::string& origin,uint64_t generation,bool success,const std::string& message){SiteResult(request,origin,generation,"clear",success,message);});site_clear_->Attach();
    document_scripts_=new tilo::DocumentScripts(browser,[this] { CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self){ self->FlushNavigation(); },CefRefPtr<Client>(this))); },[this] { StringEvent("error","message","Document-start script registration failed");CloseHost(); });
    document_scripts_->Attach();
    // Register before releasing navigation. A late private context must not
    // infer idle composition from the absence of an event it could not observe.
    composition_scripts_=new tilo::DocumentScripts(browser,[this] { ScheduleNavigation(); },
        [this] { StringEvent("error","message","Private composition observer registration failed");CloseHost(); },
        "tilo-private-text-input");
    composition_scripts_->Attach();
    composition_scripts_->Configure(R"JS(globalThis.__tiloTextComposition = (() => {
      let trusted = false, synthetic = false, unknown = false;
      let root = null;
      // An initial about:blank iframe can retain its Window when it navigates.
      // Observe the native load on that Window, then bind its current Document.
      // A late context or a document.open-removed listener remains unknown.
      const observer = new MutationObserver(records => {
        for (const record of records) {
          for (const node of record.removedNodes) if (node === root) unknown = true;
        }
        if (root !== document.documentElement) unknown = true;
        if (unknown) { root = null; observer.disconnect(); }
      });
      const loaded = event => {
        if (!event.isTrusted || unknown || !['http:', 'https:'].includes(location.protocol)) return;
        root = document.documentElement;
        if (!root) return;
        observer.observe(document, {childList: true});
        removeEventListener('DOMContentLoaded', loaded);
      };
      addEventListener('DOMContentLoaded', loaded);
      addEventListener('compositionstart', event => {
        if (event.isTrusted) trusted = true; else synthetic = true;
      }, true);
      addEventListener('compositionend', event => {
        // Page-generated end events cannot clear actual native composition.
        if (event.isTrusted) trusted = false; else synthetic = false;
      }, true);
      return Object.freeze({active: () => unknown || !root || root !== document.documentElement || trusted || synthetic});
    })())JS");
    navigation_history_=new tilo::NavigationHistory(browser,[this](const std::string& name,CefRefPtr<CefDictionaryValue> fields){Event(name,fields);});navigation_history_->Attach();
    page_automation_=new tilo::PageAutomation(browser,[this](const std::string& name,CefRefPtr<CefDictionaryValue> fields){Event(name,fields);},
        [this](double x,double y){PageClick(x,y);},[this](CefRefPtr<CefDictionaryValue> key){PageKey(key);});
    page_automation_->Attach();
    agent_log_=new tilo::AgentLog(browser,[this](const std::string& name,CefRefPtr<CefDictionaryValue> fields){Event(name,fields);});
    agent_log_->Attach();
    paint_snapshot_=new tilo::PaintSnapshot(browser,[this](const std::string& name,CefRefPtr<CefDictionaryValue> fields){Event(name,fields);});
    paint_snapshot_->Attach();
    auto d=CefDictionaryValue::Create();d->SetBool("native_edit_history",true);d->SetBool("guarded_edit_history",true);d->SetBool("native_caret_text_input",true);d->SetInt("frame_rate",kOffscreenFrameRate);d->SetString("cef_version",CEF_VERSION);d->SetString("chromium_version",std::to_string(CHROME_VERSION_MAJOR)+"."+std::to_string(CHROME_VERSION_MINOR)+"."+std::to_string(CHROME_VERSION_BUILD)+"."+std::to_string(CHROME_VERSION_PATCH));Event(id_==1?"ready":"tab_ready",d);
    TraceGeometry("ready",id_);
    browser_->GetHost()->WasHidden(!visible_);
    if(closing) browser_->GetHost()->CloseBrowser(true);
  }
  bool DoClose(CefRefPtr<CefBrowser>) override { return false; }
  void OnBeforeClose(CefRefPtr<CefBrowser> browser) override {
    if(!browser_ || !browser->IsSame(browser_))return;
    TraceGeometry("before-close",id_);
    InvalidateCredentials();
    request_filter->Remove(browser->GetIdentifier());
    if(site_clear_)site_clear_->Detach();
    site_clear_=nullptr;
    capabilities_->Close();
    if(navigation_history_)navigation_history_->Detach();
    navigation_history_=nullptr;
    if(page_automation_)page_automation_->Detach();
    page_automation_=nullptr;
    if(agent_log_)agent_log_->Detach();
    agent_log_=nullptr;
    if(paint_snapshot_)paint_snapshot_->Detach();
    paint_snapshot_=nullptr;
    if(startup_diagnostics_)startup_diagnostics_->Detach();
    startup_diagnostics_=nullptr;
    if(document_scripts_)document_scripts_->Detach();
    document_scripts_=nullptr;
    if(composition_scripts_)composition_scripts_->Detach();
    composition_scripts_=nullptr;
    browser_=nullptr;
    download_owners.Destroy(id_);
    Event("closed");clients.erase(id_);PublishDownloadRuntime();
    if(closing && clients.empty())CefQuitMessageLoop();
  }
  bool OnBeforeBrowse(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,CefRefPtr<CefRequest> request,bool,bool redirect) override {
    if(frame->IsMain()) { if(navigation_dispatched_ && (navigation_target_.empty() || navigation_started_ || request->GetURL()==navigation_target_)) { navigation_started_=true; navigation_urls_.insert(request->GetURL().ToString()); } else if(!redirect) { navigation_busy_=true; } TraceGeometry("before-browse",id_);if(page_automation_)page_automation_->NavigationStarted();InvalidateCredentials();request_filter->Begin(browser->GetIdentifier(),request->GetURL());capabilities_->Close(); }return false;
  }
  bool OnBeforePopup(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame>,int,const CefString& url,const CefString&,cef_window_open_disposition_t disposition,bool,const CefPopupFeatures&,CefWindowInfo&,CefRefPtr<CefClient>&,CefBrowserSettings&,CefRefPtr<CefDictionaryValue>&,bool*) override {
    OpenTab(url,disposition==CEF_WOD_NEW_BACKGROUND_TAB);return true;
  }
  void OnAddressChange(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,const CefString& url) override { if(frame->IsMain()) { if(navigation_dispatched_ && (navigation_target_.empty() || navigation_started_ || url==navigation_target_)) { navigation_started_=true; navigation_urls_.insert(url.ToString()); } main_navigation_finished_=false;++favicon_generation_;std::optional<std::pair<std::string,std::pair<uint64_t,bool>>> pending;if(pending_main_media_token_){const auto key=frame->GetIdentifier().ToString();auto found=media_frames_.find(key);if(found!=media_frames_.end() && found->second.first==*pending_main_media_token_)pending=std::make_pair(key,std::make_pair(found->second.first,false));}media_frames_.clear();if(pending)media_frames_.insert(*pending);pending_main_media_token_.reset();media_limit_reported_=false;last_media_playing_.reset();navigation_history_->Invalidate();StringEvent("uri","url",url);PublishMediaState(false); } }
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
  void CompleteMainNavigation(const std::string& uri,int status) {
    // A failed frame may subsequently finish its error document. Publishing
    // success there would overwrite the queued failure in the native bridge.
    if(main_navigation_finished_ || (failed_main_navigation_ && *failed_main_navigation_==uri)) return;
    main_navigation_finished_=true;
    auto loaded=CefDictionaryValue::Create();loaded->SetString("url",uri);loaded->SetInt("status",status);Event("loaded",loaded);
  }
  void OnLoadingStateChange(CefRefPtr<CefBrowser> browser,bool loading,bool back,bool forward) override {
    auto d=CefDictionaryValue::Create();d->SetBool("back",back);d->SetBool("forward",forward);Event("history",d);
    if(loading) { navigation_busy_=true; main_navigation_finished_=false; failed_main_navigation_.reset(); return; }
    // OnLoadEnd excludes same-document history; overall loading settles it.
    CompleteMainNavigation(browser->GetMainFrame()->GetURL().ToString(),0);
    if(!navigation_dispatched_ || navigation_started_ || navigation_aborting_) {
      const auto uri=browser->GetMainFrame()->GetURL().ToString();
      if(navigation_owner_) NavigationOutcome(*navigation_owner_,uri,navigation_error_);
      navigation_owner_.reset();navigation_dispatched_=false;navigation_started_=false;navigation_busy_=false;navigation_aborting_=false;bootstrap_settled_=true;navigation_urls_.clear();navigation_error_.reset();
      ScheduleNavigation();
    }
  }
  void OnLoadStart(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,TransitionType) override {
    if(!frame->IsMain()) return;
    main_navigation_finished_=false;
    InvalidateCredentials();
    TraceGeometry("load-start",id_);
    credential_committed_origin_=tilo::Origin(frame->GetURL());
  }
  void OnLoadEnd(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,int status) override {
    if(!frame->IsMain()) return;
    if(startup_diagnostics_ && tilo::Origin(frame->GetURL()).starts_with("https://"))startup_diagnostics_->Loaded();
    OfferCredentialContext(frame);pending_main_media_token_.reset();PublishZoom();
    CompleteMainNavigation(frame->GetURL().ToString(),status);SendScripts();
    if(page_automation_)page_automation_->NavigationFinished();
  }
  void OnLoadError(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,ErrorCode code,const CefString& text,const CefString& url) override {
    if(frame->IsMain()) { failed_main_navigation_=url.ToString(); InvalidateCredentials(); }
    if(!frame->IsMain()) return;
    if(navigation_owner_ && navigation_started_ && navigation_urls_.count(url.ToString())) navigation_error_=text.ToString();
    if(code!=ERR_ABORTED) {auto d=CefDictionaryValue::Create();d->SetString("url",url);d->SetString("message",text);Event("error",d);}
    if(page_automation_)page_automation_->NavigationFinished();
  }
  void OnPopupShow(CefRefPtr<CefBrowser>,bool show) override { popup_visible_=show;if(!show) { popup_.clear(); EmitFrame(); } }
  void OnPopupSize(CefRefPtr<CefBrowser>,const CefRect& rect) override { popup_rect_=rect; }
  void OnPaint(CefRefPtr<CefBrowser> browser,PaintElementType type,const RectList&,const void* buffer,int width,int height) override {
    if(!visible_) return;
    auto size=tilo::FrameSize(width,height);if(!size) return;
    if(type==PET_VIEW && !geometry_.PaintMatches(width,height)) {
      TraceGeometry("paint-stale",id_,width,height,geometry_.scale());
      if(!stale_paint_recovery_pending_) {
        stale_paint_recovery_pending_=true;
        // A paint invalidation can only repaint the renderer's old viewport.
        // Reassert the latest geometry after this paint callback completes;
        // rapid scale/layout changes may race an in-flight resize acknowledgement.
        CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self) {
          if(!self->browser_ || !self->visible_ || !self->stale_paint_recovery_pending_)return;
          TraceGeometry("stale-geometry-reassert",self->id_);
          auto host=self->browser_->GetHost();
          host->NotifyScreenInfoChanged();host->WasResized();host->Invalidate(PET_VIEW);
        },CefRefPtr<Client>(this)));
      }
      return;
    }
    if(type==PET_VIEW) { stale_paint_recovery_pending_=false;pixels_.assign(static_cast<const char*>(buffer),size);pixel_width_=width;pixel_height_=height; }
    else { popup_.assign(static_cast<const char*>(buffer),size);popup_width_=width;popup_height_=height; }
    EmitFrame();
  }
  void EmitFrame() {
    if(!visible_ || pixels_.empty()) return;
    TraceGeometry("frame-published",id_,pixel_width_,pixel_height_,geometry_.scale());
    std::string body; tilo::AppendU32(body,pixel_width_);tilo::AppendU32(body,pixel_height_);body+=pixels_;
    if(popup_visible_ && !popup_.empty()) {
      int x=static_cast<int>(std::round(popup_rect_.x*geometry_.scale())),y=static_cast<int>(std::round(popup_rect_.y*geometry_.scale()));
      for(int row=0;row<popup_height_;++row) {
        int dest_y=y+row;if(dest_y<0 || dest_y>=pixel_height_) continue;
        int start=std::max(0,-x),end=std::min(popup_width_,pixel_width_-x);
        if(end>start) std::memcpy(body.data()+8+(size_t(dest_y)*pixel_width_+x+start)*4,popup_.data()+(size_t(row)*popup_width_+start)*4,size_t(end-start)*4);
      }
    }
    if(id_==1) output->Send(2,std::move(body));
    else {std::string routed;tilo::AppendU32(routed,id_);routed+=body;output->Send(3,std::move(routed));}
  }
  bool OnProcessMessageReceived(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,CefProcessId source,CefRefPtr<CefProcessMessage> message) override {
    if(source!=PID_RENDERER) return false;
    if(message->GetName()=="tilo.credential_result") {
      ReceiveCredentialResult(browser,frame,message->GetArgumentList());return true;
    }
    if(message->GetName()=="tilo.credential_context" || message->GetName()=="tilo.credential_context_closed") {
      if(!browser_ || !browser || !browser->IsSame(browser_) || !frame ||
         !frame->IsMain() || !frame->IsValid() || transfer_owner_) return true;
      TraceGeometry("credential-context-received",id_);
      auto args=message->GetArgumentList();
      if(args->GetSize()!=3 || args->GetType(0)!=VTYPE_STRING ||
         args->GetType(1)!=VTYPE_STRING || args->GetType(2)!=VTYPE_STRING ||
         args->GetString(0).ToString()!=std::to_string(credential_generation_)) return true;
      const auto context=args->GetString(1).ToString();
      if(context.empty() || context.size()>128 || args->GetString(2)!=frame->GetURL()) return true;
      if(message->GetName()=="tilo.credential_context_closed") {
        if(credential_identity_ && credential_identity_->context==context) InvalidateCredentials();
        return true;
      }
      tilo::CredentialIdentity identity{browser->GetIdentifier(),frame->GetIdentifier().ToString(),
          tilo::Origin(frame->GetURL()),context,credential_generation_};
      if(!credential_handshake_.Accept(identity) || !CredentialPageSecure(frame)) return true;
      if(credential_document_.Commit(identity,true,true,false)) {credential_identity_=identity;TraceGeometry("credential-context-committed",id_);}
      return true;
    }
    if(message->GetName()=="tilo.media_context" && frame && browser_) {const auto key=frame->GetIdentifier().ToString();const auto token=static_cast<uint64_t>(message->GetArgumentList()->GetDouble(0));if(frame->IsMain())pending_main_media_token_=token;auto found=media_frames_.find(key);if(found!=media_frames_.end()) found->second={token,false};else if(media_frames_.size()<kMaxMediaFrames) media_frames_.emplace(key,std::make_pair(token,false));if(media_frames_.size()==kMaxMediaFrames && !media_limit_reported_){media_limit_reported_=true;auto d=CefDictionaryValue::Create();d->SetInt("tracked_frames",static_cast<int>(media_frames_.size()));Event("media_frame_limit",d);}return true; }
    if(message->GetName()=="tilo.media_context_closed" && frame && browser_) {auto found=media_frames_.find(frame->GetIdentifier().ToString());if(found!=media_frames_.end() && found->second.first==static_cast<uint64_t>(message->GetArgumentList()->GetDouble(0))){const bool was_playing=found->second.second;media_frames_.erase(found);if(was_playing)PublishMediaState();}return true;}
    if(message->GetName()=="tilo.media_state" && frame && browser_) { auto found=media_frames_.find(frame->GetIdentifier().ToString());const auto token=static_cast<uint64_t>(message->GetArgumentList()->GetDouble(1));if(found!=media_frames_.end() && found->second.first==token) {found->second.second=message->GetArgumentList()->GetBool(0);PublishMediaState();}return true; }
    if(message->GetName()!="tilo.config") return false;
    SendFilters(frame,message->GetArgumentList()->GetDouble(0));SendScripts(frame);return true;
  }
  void PublishMediaState(std::optional<bool> forced=std::nullopt) {
    if(!browser_) return;bool playing=forced.value_or(std::any_of(media_frames_.begin(),media_frames_.end(),[](const auto& entry){return entry.second.second;}));
    if(!forced && last_media_playing_==playing) return;last_media_playing_=playing;
    auto d=CefDictionaryValue::Create();d->SetString("page_url",browser_->GetMainFrame()->GetURL());d->SetInt("document_generation",static_cast<int>(favicon_generation_ & 0x7fffffff));d->SetBool("playing",playing);Event("media_state",d);
  }
  void SendFilters(CefRefPtr<CefFrame> frame,double token=0) {
    if(!frame) return;
    auto message=CefProcessMessage::Create("tilo.filters");auto args=message->GetArgumentList();
    auto state=request_filter->Snapshot();args->SetString(0,state->rules);args->SetBool(1,state->enabled);
    auto exceptions=CefListValue::Create();for(const auto& origin:state->exceptions)exceptions->SetString(exceptions->GetSize(),origin);args->SetList(2,exceptions);args->SetString(3,browser_?tilo::Origin(browser_->GetMainFrame()->GetURL()):std::string());args->SetString(4,frame->GetURL());args->SetDouble(5,token);
    frame->SendProcessMessage(PID_RENDERER,message);
  }
  void RefreshFilters() {if(!browser_)return;UpdateScriptPolicy();std::vector<CefString> ids;browser_->GetFrameIdentifiers(ids);for(const auto& id:ids)SendFilters(browser_->GetFrameByIdentifier(id));}
  void SiteResult(int request,const std::string& origin,uint64_t generation,const std::string& action,bool success,const std::string& message) {
    auto d=CefDictionaryValue::Create();d->SetInt("request_id",request);d->SetString("origin",origin);d->SetDouble("generation",generation);d->SetString("action",action);d->SetBool("success",success);d->SetString("message",message);Event("site_result",d);
  }
  bool SiteCommand(CefRefPtr<CefDictionaryValue> command) {
    const auto origin=command->GetString("origin").ToString(),op=command->GetString("op").ToString();const int request=command->GetInt("request_id");
    auto document=request_filter->Document(browser_->GetIdentifier());const auto actual=tilo::Origin(browser_->GetMainFrame()->GetURL());
    const auto action=op=="site_status"?"status":command->GetString("action").ToString();
    // Action replies echo their requested document, including stale failures,
    // so chrome can finish the matching request instead of waiting for timeout.
    const auto reply_generation=op=="site_action"?static_cast<uint64_t>(Number(command,"generation")):(document?document->generation:0);
    if(origin.empty() || origin!=actual || !document || document->origin!=actual || (op=="site_action" && Number(command,"generation")!=document->generation)) {
      SiteResult(request,origin,reply_generation,action,false,"Page changed; reopen site information");return false;
    }
    if(op=="site_action") {
      if(action=="reload") {
        // Command has already waited for document-start registration, then this
        // method rechecked the original scope. The UI thread cannot navigate
        // between that check and the ordinary reload command below.
        SiteResult(request,origin,document->generation,action,true,"Reloading this page");
        browser_->Reload();return true;
      }
      if(action=="clear") {
        if(site_clear_->Busy())SiteResult(request,origin,document->generation,action,false,"A storage clear is already running");
        else site_clear_->Clear(request,origin,document->generation);
        return false;
      }
      if(action=="revoke") {
        capabilities_->Close();auto context=browser_->GetHost()->GetRequestContext();for(const auto& permission:tilo::kSitePermissions)context->SetContentSetting(origin,origin,permission.type,CEF_CONTENT_SETTING_VALUE_BLOCK);
        SiteResult(request,origin,document->generation,action,true,"Permissions blocked. Reload to end existing media sessions.");
      } else if(action=="enable" || action=="disable") {
        auto origins=request_filter->Snapshot()->exceptions;if(action=="disable")origins.insert(origin);else origins.erase(origin);
        if(origins.size()>1024) {SiteResult(request,origin,document->generation,action,false,"Site exception limit reached");return false;}
        request_filter->Exceptions(origins);PublishFilters();SiteResult(request,origin,document->generation,action,true,"Protection preference changed");
      }
      return false;
    }
    auto state=request_filter->Snapshot();auto d=CefDictionaryValue::Create();d->SetInt("request_id",request);d->SetString("origin",actual);d->SetDouble("generation",document->generation);d->SetDouble("blocked",std::min<uint64_t>(document->blocked.load(),9007199254740991ULL));d->SetBool("global_enabled",state->enabled);d->SetBool("excepted",state->exceptions.count(actual));d->SetBool("effective",state->enabled && !state->exceptions.count(actual));
    auto entry=browser_->GetHost()->GetVisibleNavigationEntry();auto ssl=entry && tilo::Origin(entry->GetURL())==actual?entry->GetSSLStatus():nullptr;
    const bool secure=ssl && ssl->IsSecureConnection() && !CefIsCertStatusError(ssl->GetCertStatus());
    d->SetString("security",actual.starts_with("http:")?"insecure":secure?"secure":"unverified");d->SetInt("certificate_status",ssl?ssl->GetCertStatus():0);d->SetInt("tls_version",ssl?ssl->GetSSLVersion():0);d->SetInt("content_status",ssl?ssl->GetContentStatus():0);
    std::string subject,issuer;double expires=0;
    if(ssl)if(auto certificate=ssl->GetX509Certificate()){subject=certificate->GetSubject()->GetDisplayName();issuer=certificate->GetIssuer()->GetDisplayName();expires=certificate->GetValidExpiry().val;}
    d->SetString("subject",subject.substr(0,512));d->SetString("issuer",issuer.substr(0,512));d->SetDouble("expires",expires);d->SetDictionary("permissions",tilo::PermissionStatus(browser_->GetHost()->GetRequestContext(),actual));Event("site_status",d);
    return false;
  }
  void SendScripts(CefRefPtr<CefFrame> frame=nullptr) {
    if(!frame) { if(!browser_) return;frame=browser_->GetMainFrame(); }
    auto message=CefProcessMessage::Create("tilo.scripts");auto args=message->GetArgumentList();
    args->SetString(0,ScriptSource());
    frame->SendProcessMessage(PID_RENDERER,message);
  }
  std::string ScriptSource() {
    auto scripts=scripts_;auto policy=request_filter->Snapshot();if(!blocking_scripts_.empty())scripts.push_back(tilo::BlockingScriptSource(blocking_scripts_,policy->enabled,policy->exceptions));return tilo::ScriptSource(scripts);
  }
  void UpdateScriptPolicy() {if(document_scripts_){document_scripts_->Configure(ScriptSource());SendScripts();}}
  void NavigationOutcome(int request,const std::string& uri,const std::optional<std::string>& error) {
    auto d=CefDictionaryValue::Create();d->SetInt("request_id",request);d->SetString("url",uri);if(error)d->SetString("error",*error);Event("navigation_outcome",d);
  }
  void ScheduleNavigation() {
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self){self->FlushNavigation();},CefRefPtr<Client>(this)));
  }
  void FlushNavigation() {
    TraceGeometry("navigation-flush",id_);
    if(!browser_ || closing || !document_scripts_ || document_scripts_->Busy() || !composition_scripts_ || composition_scripts_->Busy() || !bootstrap_settled_ || navigation_busy_ || pending_navigation_.empty()) return;
    auto command=pending_navigation_.front();pending_navigation_.erase(pending_navigation_.begin());
    navigation_dispatched_=true;navigation_started_=false;navigation_busy_=true;navigation_urls_.clear();navigation_error_.reset();
    const bool scoped=command->GetString("op")=="scoped_navigation";
    navigation_owner_=scoped?std::optional<int>(command->GetInt("request_id")):std::nullopt;
    navigation_target_.clear();
    std::string op=scoped?command->GetString("command").ToString():command->GetString("op").ToString();
    auto frame=browser_->GetMainFrame();
    if(op=="navigate") {navigation_target_=command->GetString("url");CefURLParts parts;if(CefParseURL(navigation_target_,parts))navigation_target_=CefString(&parts.spec).ToString();TraceGeometry("navigate-load-url",id_);frame->LoadURL(command->GetString("url"));}
    else if(op=="back" && browser_->CanGoBack())browser_->GoBack();
    else if(op=="forward" && browser_->CanGoForward())browser_->GoForward();
    else if(op=="reload")browser_->Reload();
    else if(op=="site_action" && SiteCommand(command)) {}
    else if(op=="history_entry" && navigation_history_->Navigate(scoped?command->GetInt("history_request_id"):command->GetInt("request_id"),command->GetInt("id"),[self=CefRefPtr<Client>(this),owner=navigation_owner_]{if(owner && self->navigation_owner_==owner)self->RefuseNavigation("History navigation was refused");})) {}
    else { if(navigation_owner_)NavigationOutcome(*navigation_owner_,frame->GetURL(),"Navigation is unavailable");navigation_owner_.reset();navigation_dispatched_=false;navigation_busy_=false;ScheduleNavigation(); }
  }
  bool DeferNavigation(CefRefPtr<CefDictionaryValue> command) {
    if(pending_navigation_.size()>=256) {if(command->GetString("op")=="scoped_navigation")NavigationOutcome(command->GetInt("request_id"),browser_->GetMainFrame()->GetURL(),"Navigation queue exhausted");else {StringEvent("error","message","Document-start navigation queue exhausted");CloseHost();}return true;}
    TraceGeometry("navigation-queued",id_);pending_navigation_.push_back(command->Copy(false));
    // An explicit replacement stops the old lifecycle; its remaining callbacks
    // finish without owning the newly queued operation.
    if(bootstrap_settled_ && navigation_busy_) {
      if(navigation_owner_)NavigationOutcome(*navigation_owner_,browser_->GetMainFrame()->GetURL(),"Navigation was superseded");
      navigation_owner_.reset();AbortNavigation();
    }
    ScheduleNavigation();return true;
  }
  void RefuseNavigation(const std::string& error) {
    if(navigation_owner_)NavigationOutcome(*navigation_owner_,browser_->GetMainFrame()->GetURL(),error);
    navigation_owner_.reset();navigation_dispatched_=false;navigation_started_=false;navigation_busy_=false;ScheduleNavigation();
  }
  void AbortNavigation() {
    navigation_aborting_=true;browser_->StopLoad();
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self){
      if(!self->browser_ || !self->navigation_aborting_ || self->browser_->IsLoading())return;
      self->navigation_aborting_=false;self->navigation_busy_=false;self->navigation_dispatched_=false;self->navigation_started_=false;self->ScheduleNavigation();
    },CefRefPtr<Client>(this)));
  }
  void CancelNavigation(int request) {
    std::erase_if(pending_navigation_,[request](const auto& command){return command->GetString("op")=="scoped_navigation" && command->GetInt("request_id")==request;});
    if(navigation_owner_==request) {navigation_owner_.reset();AbortNavigation();}
  }
  void Command(CefRefPtr<CefDictionaryValue> d) {
    CEF_REQUIRE_UI_THREAD();
    std::string op=d->GetString("op");
    if(op=="shutdown") { CloseHost();return; }
    if(!browser_ || closing) return;
    if(transfer_owner_) {if(d->GetString("op")=="close_tab" && !downloads->Detach(id_)) CloseNative();return;}
    if(op=="cancel_navigation") {CancelNavigation(d->GetInt("request_id"));return;}
    if(op=="scoped_navigation" && d->GetInt("request_id")<=0) {CloseHost();return;}
    if(op=="site_status" || op=="site_action") {
      // Preserve the full scoped command across registration, rather than
      // queueing a generic reload that could act on a later document.
      if(op=="site_action" && d->GetString("action")=="reload" && DeferNavigation(d))return;
      SiteCommand(d);return;
    }
    if(op=="site_exceptions") {std::set<std::string> origins;auto list=d->GetList("origins");for(size_t i=0;i<list->GetSize();++i)origins.insert(list->GetString(i));request_filter->Exceptions(origins);PublishFilters();return;}
    if(op=="audio_muted") { browser_->GetHost()->SetAudioMuted(d->GetBool("muted"));auto state=CefDictionaryValue::Create();state->SetBool("muted",browser_->GetHost()->IsAudioMuted());Event("audio_muted",state);return; }
    if((op=="scoped_navigation" || op=="navigate" || op=="back" || op=="forward" || op=="reload" || op=="history_entry") && DeferNavigation(d))return;
    auto host=browser_->GetHost();auto frame=browser_->GetMainFrame();
    if(op=="visibility") {
      const bool was_visible=visible_;
      visible_=d->GetBool("visible");
      credential_document_.SetVisible(visible_);
      if(!visible_) CancelCredentialRequests();
      host->WasHidden(!visible_);host->SetFocus(visible_);
      if(!visible_ && context_callback_) context_callback_->Cancel();
      if(visible_) {
        host->Invalidate(PET_VIEW);
        // A background load may have completed without an eligible handshake.
        if(!was_visible && !credential_identity_) OfferCredentialContext(browser_->GetMainFrame());
      } else {pixels_.clear();popup_.clear();}
    }
    else if(op=="context_action") {
      if(context_callback_ && d->GetInt("id")==context_id_) {
        int command=d->GetInt("command");auto callback=context_callback_;context_callback_=nullptr;
        if(command && context_commands_.count(command)) callback->Continue(command,EVENTFLAG_NONE);else callback->Cancel();
      }
    }
    else if(op=="credential_action") {CredentialCommand(d);}
    else if(op=="text_input") {
      if(!visible_ && d->GetString("action")!="cancel_text_input") {
        auto fields=CefDictionaryValue::Create();fields->SetInt("request_id",d->GetInt("request_id"));
        fields->SetBool("ok",false);fields->SetString("code","unavailable");
        fields->SetString("message","The browser view is hidden");Event("text_input_result",fields);
      } else page_automation_->Request(d);
    }
    else if(op=="page") {
      // Whatever an agent reads or does next, its logs start here.
      const auto action=d->GetString("action").ToString();
      if(action=="console" || action=="network") agent_log_->Request(d);
      else { agent_log_->Activate();page_automation_->Request(d); }
    }
    else if(op=="paint_capture") paint_snapshot_->Capture(d);
    else if(op=="paint_resource") paint_snapshot_->Resource(d);
    else if(op=="try_close_tab") {DetachVisible();}
    else if(op=="close_tab") {CloseNative();}
    else if(op=="navigate") {TraceGeometry("navigate-load-url",id_);frame->LoadURL(d->GetString("url"));}
    else if(op=="navigation_history") navigation_history_->Request(d->GetInt("request_id"),d->GetString("direction") == "forward");
    else if(op=="history_entry") navigation_history_->Navigate(d->GetInt("request_id"),d->GetInt("id"));
    else if(op=="back") browser_->GoBack();
    else if(op=="forward") browser_->GoForward();
    else if(op=="reload") browser_->Reload();
    else if(op=="stop") {
      for(const auto& command:pending_navigation_)if(command->GetString("op")=="scoped_navigation")NavigationOutcome(command->GetInt("request_id"),frame->GetURL(),"Navigation was stopped");
      pending_navigation_.clear();
      if(navigation_owner_)NavigationOutcome(*navigation_owner_,frame->GetURL(),"Navigation was stopped");
      navigation_owner_.reset();AbortNavigation();
    }
    else if(op=="exit_fullscreen") host->ExitFullscreen(true);
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
      if(!std::isfinite(scale)||scale<0.25||scale>8||!tilo::FrameSize(w,h)) { CloseHost(); return; }
      // IPC sizes and pointer coordinates are physical pixels; CEF uses DIP.
      const auto effect=geometry_.ResizePhysical(w,h,scale);
      TraceGeometry("resize-applied",id_,w,h,scale);
      stale_paint_recovery_pending_=false;
      if(effect==tilo::ResizeEffect::Repaint) { host->NotifyScreenInfoChanged();host->WasResized();host->Invalidate(PET_VIEW);return; }
      popup_.clear();pixels_.clear();host->NotifyScreenInfoChanged();host->WasResized();host->Invalidate(PET_VIEW);
    } else if(op=="mouse_move" || op=="mouse_button" || op=="scroll") {
      CefMouseEvent event;event.x=static_cast<int>(Number(d,"x")/geometry_.scale());event.y=static_cast<int>(Number(d,"y")/geometry_.scale());event.modifiers=mouse_modifiers_|key_modifiers_;
      if(op=="mouse_move") host->SendMouseMoveEvent(event,false);
      else if(op=="scroll") host->SendMouseWheelEvent(event,static_cast<int>(Number(d,"dx")/geometry_.scale()),static_cast<int>(Number(d,"dy")/geometry_.scale()));
      else { std::string button=d->GetString("button");auto type=button=="right"?MBT_RIGHT:button=="middle"?MBT_MIDDLE:MBT_LEFT;bool down=d->GetBool("down");
        int flag=type==MBT_RIGHT?EVENTFLAG_RIGHT_MOUSE_BUTTON:type==MBT_MIDDLE?EVENTFLAG_MIDDLE_MOUSE_BUTTON:EVENTFLAG_LEFT_MOUSE_BUTTON;
        if(down) mouse_modifiers_|=flag;else mouse_modifiers_&=~flag; event.modifiers=mouse_modifiers_|key_modifiers_;host->SendMouseClickEvent(event,type,!down,1); }
    } else if(op=="modifiers") key_modifiers_=(d->GetBool("ctrl")?EVENTFLAG_CONTROL_DOWN:0)|(d->GetBool("shift")?EVENTFLAG_SHIFT_DOWN:0)|(d->GetBool("alt")?EVENTFLAG_ALT_DOWN:0)|(d->GetBool("meta")?EVENTFLAG_COMMAND_DOWN:0);
    else if(op=="edit_history") {
      auto frame=browser_->GetFocusedFrame();
      if(frame) { if(d->GetString("direction")=="undo") frame->Undo();else frame->Redo(); }
    }
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
    const auto mapping=tilo::MapKey(key);
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
  // Page actions deliver input exactly as a person's pointer and keyboard do.
  // Coordinates arrive in CSS pixels; CEF's view space is zoomed DIPs.
  void PageClick(double x,double y) {
    if(!browser_)return;auto host=browser_->GetHost();const double zoom=std::pow(1.2,host->GetZoomLevel());
    CefMouseEvent event;event.x=static_cast<int>(std::lround(x*zoom));event.y=static_cast<int>(std::lround(y*zoom));event.modifiers=key_modifiers_;
    host->SendMouseMoveEvent(event,false);
    event.modifiers|=EVENTFLAG_LEFT_MOUSE_BUTTON;host->SendMouseClickEvent(event,MBT_LEFT,false,1);
    event.modifiers&=~EVENTFLAG_LEFT_MOUSE_BUTTON;host->SendMouseClickEvent(event,MBT_LEFT,true,1);
  }
  void PageKey(CefRefPtr<CefDictionaryValue> key) {
    if(!browser_)return;const int held=key_modifiers_;
    // Keys reach only a focused view; the page's own focused element is kept.
    browser_->GetHost()->SetFocus(true);
    key->SetBool("down",true);Key(key);key->SetBool("down",false);Key(key);
    key_modifiers_=held;
  }
  CefRefPtr<CefBrowser> browser_;
  bool detached() const {return transfer_owner_;}
  bool accepts_downloads() const {return browser_ && !transfer_owner_ && !native_closing_;}
  void CloseNative() {if(!browser_ || native_closing_)return;InvalidateCredentials();native_closing_=true;browser_->GetHost()->CloseDevTools();browser_->GetHost()->CloseBrowser(true);}
  void DetachVisible() {
    if(transfer_owner_ || native_closing_)return;const bool retained=downloads->Detach(id_);if(!retained){CloseNative();return;}
    // Every busy visible owner reserved this slot at admission, so detaching
    // cannot displace accepted work even while earlier owners await destruction.
    if(!download_owners.Detach(id_)){Event("close_blocked");return;}
    InvalidateCredentials();
    transfer_owner_=true;visible_=false;auto host=browser_->GetHost();host->WasHidden(true);host->SetFocus(false);
    if(context_callback_){context_callback_->Cancel();context_callback_=nullptr;}capabilities_->Close();pixels_.clear();popup_.clear();Event("tab_detached");PublishDownloadRuntime();
  }
 private:
  struct CredentialPending {
    int request=0;
    tilo::CredentialOperation operation;
    tilo::CredentialIdentity identity;
    std::string action, form, account;
  };
  struct CredentialStorePending {
    CredentialPending pending;
    std::shared_ptr<tilo::CredentialCancellation> cancellation;
    bool timed_out=false;
  };
  static std::string CredentialStatusMessage(tilo::CredentialStatus status) {
    switch(status) {
      case tilo::CredentialStatus::Ok:return "Done";
      case tilo::CredentialStatus::Cancelled:return "Cancelled";
      case tilo::CredentialStatus::NotFound:return "Saved account no longer exists";
      case tilo::CredentialStatus::Denied:return "Keychain access was denied or is locked";
      case tilo::CredentialStatus::Capacity:return "This site has reached the 64-account capacity; new accounts cannot be saved";
      case tilo::CredentialStatus::Exists:return "This account is already saved; choose Update";
      case tilo::CredentialStatus::Invalid:return "Credential request or stored entry is invalid";
      case tilo::CredentialStatus::Unavailable:return "Keychain is unavailable";
    }
    return "Keychain is unavailable";
  }
  void StartCredentialStore(int request,const std::string& action,tilo::CredentialRequest operation,const std::string& form={}) {
    if(!credential_worker || private_mode || !credential_identity_ || !CredentialPageSecure(browser_->GetMainFrame())) {
      CredentialEvent(request,action,false,"Credential storage is unavailable");return;
    }
    const auto kind=operation.operation;const auto account=operation.account;
    auto id=credential_document_.Begin(kind,*credential_identity_,tilo::CredentialDocument::Clock::now());
    if(!id || credential_store_pending_.size()>=16){
      if(id)credential_document_.Take(*id,kind,*credential_identity_,tilo::CredentialDocument::Clock::now());
      CredentialEvent(request,action,false,"Too many pending Keychain requests");return;
    }
    operation.origin=credential_identity_->origin;
    const int tab=id_,browser_id=browser_->GetIdentifier();
    auto cancellation=credential_worker->Submit(std::move(operation),[tab,browser_id,id=*id,delivery=credential_delivery](tilo::CredentialResult result,bool cancelled){
      auto outcome=std::make_shared<tilo::CredentialResult>(std::move(result));
      std::lock_guard lock(delivery->mutex);
      if(!delivery->active)return;
      // Only plain IDs and native result data cross threads. No Client/CEF
      // object is retained by a blocked OS call after its browser closes.
      CefPostTask(TID_UI,base::BindOnce([](int tab,int browser_id,uint64_t id,
          std::shared_ptr<tilo::CredentialResult> result,bool cancelled){
        auto client=clients.find(tab);
        if(client==clients.end() || !client->second->browser_ ||
           client->second->browser_->GetIdentifier()!=browser_id)return;
        client->second->FinishCredentialStore(id,std::move(*result),cancelled);
      },tab,browser_id,id,std::move(outcome),cancelled));
    });
    if(!cancellation) {
      credential_document_.Take(*id,kind,*credential_identity_,tilo::CredentialDocument::Clock::now());
      CredentialEvent(request,action,false,"Keychain queue is full or unavailable");return;
    }
    credential_store_pending_.emplace(*id,CredentialStorePending{
      {request,kind,*credential_identity_,action,form,account},cancellation,false});
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self,uint64_t id){
      auto found=self->credential_store_pending_.find(id);if(found==self->credential_store_pending_.end())return;
      found->second.timed_out=true;found->second.cancellation->Cancel();
      const auto& pending=found->second.pending;
      self->credential_document_.Take(id,pending.operation,pending.identity,tilo::CredentialDocument::Clock::now());
      self->CredentialEvent(pending.request,pending.action,false,"Keychain timed out; an operation already started may still finish. Refresh saved accounts before retrying.");
    },CefRefPtr<Client>(this),*id),30000);
  }
  void FinishCredentialStore(uint64_t id,tilo::CredentialResult result,bool cancelled) {
    auto found=credential_store_pending_.find(id);if(found==credential_store_pending_.end())return;
    auto state=std::move(found->second);credential_store_pending_.erase(found);
    const auto& pending=state.pending;
    if(!credential_identity_ || *credential_identity_!=pending.identity ||
       !CredentialPageSecure(browser_->GetMainFrame()))return;
    if(state.timed_out) {
      auto fields=CefDictionaryValue::Create();fields->SetBool("late",true);
      CredentialEvent(pending.request,pending.action,pending.action!="fill" && result.status==tilo::CredentialStatus::Ok,
                      pending.action=="fill"?"Delayed read discarded; nothing was filled":"Delayed Keychain result: "+CredentialStatusMessage(result.status)+". Refresh saved accounts.",fields);
      return;
    }
    if(!credential_document_.Take(id,pending.operation,*credential_identity_,tilo::CredentialDocument::Clock::now()) || cancelled) {
      CredentialEvent(pending.request,pending.action,false,"Request no longer active; refresh saved accounts to see the final state");return;
    }
    if(result.status==tilo::CredentialStatus::Ok && pending.action=="fill") {
      if(result.accounts.size()!=1 || result.accounts.front().id!=pending.account ||
         result.accounts.front().username.size()>1024 || result.secret.View().empty() || result.secret.View().size()>4096) {
        CredentialEvent(pending.request,pending.action,false,"Stored credential is invalid");return;
      }
      auto payload=CefDictionaryValue::Create();payload->SetString("action","fill");payload->SetString("form",pending.form);
      payload->SetString("username",result.accounts.front().username);
      std::string secret(result.secret.View());payload->SetString("password",secret);
      volatile char* bytes=secret.data();for(size_t i=0;i<secret.size();++i)bytes[i]=0;
      SendCredentialForm(pending.request,"fill",tilo::CredentialOperation::Fill,payload);return;
    }
    auto fields=CefDictionaryValue::Create();bool success=result.status==tilo::CredentialStatus::Ok;
    if(success && pending.action=="list") {
      auto accounts=CefListValue::Create();
      if(result.accounts.size()>64)success=false;
      if(success)for(const auto& account:result.accounts) {
        if(account.id.empty() || account.id.size()>128 || account.username.size()>1024){success=false;break;}
        auto item=CefDictionaryValue::Create();item->SetString("id",account.id);item->SetString("username",account.username);
        accounts->SetDictionary(accounts->GetSize(),item);
      }
      if(success)fields->SetList("accounts",accounts);
    }
    CredentialEvent(pending.request,pending.action,success,success?"Done":CredentialStatusMessage(result.status),fields);
  }
  void CancelCredentialStoreRequests() {
    for(const auto& [id,state]:credential_store_pending_) {
      state.cancellation->Cancel();
      credential_document_.Take(id,state.pending.operation,state.pending.identity,tilo::CredentialDocument::Clock::now());
    }
    credential_store_pending_.clear();
  }
  void CancelCredentialRequests() {
    CancelCredentialStoreRequests();credential_consent_.Clear();
    for(const auto& [id,pending]:credential_pending_)
      credential_document_.Take(id,pending.operation,pending.identity,tilo::CredentialDocument::Clock::now());
    credential_pending_.clear();credential_confirming_.clear();
  }
  void CredentialEvent(int request,const std::string& action,bool success,const std::string& message,
                       CefRefPtr<CefDictionaryValue> fields=nullptr) {
    if(!fields)fields=CefDictionaryValue::Create();
    fields->SetInt("request_id",request);fields->SetString("action",action);fields->SetBool("success",success);
    fields->SetString("message",message);fields->SetString("origin",credential_identity_?credential_identity_->origin:std::string());
    fields->SetString("document",credential_identity_?std::to_string(credential_identity_->generation):std::string());
    Event("credential_result",fields);
  }
  void CredentialCommand(CefRefPtr<CefDictionaryValue> command) {
    const auto action=command->GetString("action").ToString();const int request=command->GetInt("request_id");
    if(action=="cancel") {
      CancelCredentialRequests();CredentialEvent(request,action,true,"Pending requests cancelled. Operations already started may still finish; refresh saved accounts.");return;
    }
    auto frame=browser_?browser_->GetMainFrame():nullptr;
    if(!credential_identity_ || !CredentialPageSecure(frame) ||
       command->GetString("origin").ToString()!=credential_identity_->origin ||
       (action!="status" && command->GetString("document").ToString()!=std::to_string(credential_identity_->generation))) {
      CredentialEvent(request,action,false,private_mode?"Unavailable in private windows":"Page changed or secure form access is unavailable");return;
    }
    if(action=="status"){CredentialEvent(request,action,true,"Ready");return;}
    if(action=="list" || action=="delete") {
      tilo::CredentialRequest operation;operation.operation=action=="list"?tilo::CredentialOperation::List:tilo::CredentialOperation::Delete;
      if(action=="delete")operation.account=command->GetString("account").ToString();
      StartCredentialStore(request,action,std::move(operation));return;
    }
    if(action=="save" || action=="update") {
      auto captured=credential_consent_.Take(command->GetString("capture").ToString(),*credential_identity_,tilo::CredentialDocument::Clock::now());
      if(!captured){CredentialEvent(request,action,false,"Capture expired or page changed; capture the login again");return;}
      auto payload=CefDictionaryValue::Create();payload->SetString("action","validate");payload->SetString("form",captured->form);
      SendCredentialForm(request,action,action=="save"?tilo::CredentialOperation::Save:tilo::CredentialOperation::Update,
                         payload,std::move(captured),command->GetString("account").ToString());return;
    }
    if(action=="fill") {
      tilo::CredentialRequest operation;operation.operation=tilo::CredentialOperation::Fill;
      operation.account=command->GetString("account").ToString();
      StartCredentialStore(request,action,std::move(operation),command->GetString("form").ToString());return;
    }
    const auto operation=action=="capture"?tilo::CredentialOperation::Capture:tilo::CredentialOperation::List;
    auto payload=CefDictionaryValue::Create();payload->SetString("action",action);
    if(action=="capture" || action=="fill_address")payload->SetString("form",command->GetString("form"));
    if(action=="fill_address")payload->SetDictionary("profile",command->GetDictionary("profile")->Copy(false));
    SendCredentialForm(request,action,operation,payload);
  }
  void SendCredentialForm(int request,const std::string& action,tilo::CredentialOperation operation,
                          CefRefPtr<CefDictionaryValue> payload,
                          std::optional<tilo::CapturedCredential> captured=std::nullopt,const std::string& account={}) {
    if(!credential_identity_ || !browser_ || !CredentialPageSecure(browser_->GetMainFrame())) {
      CredentialEvent(request,action,false,"Page changed; request cancelled");return;
    }
    auto id=credential_document_.Begin(operation,*credential_identity_,tilo::CredentialDocument::Clock::now());
    if(!id){CredentialEvent(request,action,false,"Too many pending requests");return;}
    credential_pending_.emplace(*id,CredentialPending{request,operation,*credential_identity_,action,payload->GetString("form").ToString(),account});
    if(captured)credential_confirming_.emplace(*id,std::move(*captured));
    auto message=CefProcessMessage::Create("tilo.credential_action");auto args=message->GetArgumentList();
    args->SetString(0,std::to_string(credential_identity_->generation));args->SetString(1,credential_identity_->context);
    args->SetString(2,std::to_string(*id));args->SetDictionary(3,payload);
    browser_->GetMainFrame()->SendProcessMessage(PID_RENDERER,message);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self,uint64_t id){
      auto found=self->credential_pending_.find(id);if(found==self->credential_pending_.end())return;
      const auto pending=found->second;self->credential_pending_.erase(found);self->credential_confirming_.erase(id);
      self->credential_document_.Take(id,pending.operation,pending.identity,tilo::CredentialDocument::Clock::now());
      self->CredentialEvent(pending.request,pending.action,false,"Request timed out; try again");
    },CefRefPtr<Client>(this),*id),30000);
  }
  void ReceiveCredentialResult(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,CefRefPtr<CefListValue> args) {
    if(!browser_ || !browser || !browser->IsSame(browser_) || !credential_identity_ ||
       !frame || !frame->IsMain() || frame->GetIdentifier().ToString()!=credential_identity_->frame ||
       !CredentialPageSecure(frame) || args->GetSize()!=4 ||
       args->GetType(0)!=VTYPE_STRING || args->GetType(1)!=VTYPE_STRING ||
       args->GetType(2)!=VTYPE_STRING || args->GetType(3)!=VTYPE_DICTIONARY ||
       args->GetString(0).ToString()!=std::to_string(credential_identity_->generation) ||
       args->GetString(1).ToString()!=credential_identity_->context ||
       !tilo::ValidCredentialCounter(args->GetString(2).ToString()))return;
    const auto id=std::stoull(args->GetString(2).ToString());
    auto found=credential_pending_.find(id);if(found==credential_pending_.end())return;
    const auto pending=found->second;credential_pending_.erase(found);
    if(!credential_document_.Take(id,pending.operation,*credential_identity_,tilo::CredentialDocument::Clock::now())) {
      credential_confirming_.erase(id);CredentialEvent(pending.request,pending.action,false,"Request expired; try again");return;
    }
    auto value=args->GetDictionary(3);auto fields=CefDictionaryValue::Create();
    bool success=value->GetType("success")==VTYPE_BOOL && value->GetBool("success");
    if(pending.action=="save" || pending.action=="update") {
      auto captured=credential_confirming_.extract(id);
      if(!success || captured.empty()){CredentialEvent(pending.request,pending.action,false,"Original form changed; capture the login again");return;}
      tilo::CredentialRequest operation;operation.operation=pending.operation;
      operation.username=std::move(captured.mapped().username);operation.secret=std::move(captured.mapped().secret);
#if defined(OS_MAC)
      operation.account=pending.action=="save"?tilo::CredentialAccountId(operation.username):pending.account;
#else
      operation.account=pending.account;
#endif
      StartCredentialStore(pending.request,pending.action,std::move(operation));return;
    }
    if(pending.action=="fill" || pending.action=="fill_address") {
      CredentialEvent(pending.request,pending.action,success,success?"Filled":"Form changed or is unsupported");return;
    }
    if(success && pending.action=="capture") {
      success=value->GetType("username")==VTYPE_STRING && value->GetString("username").length()<=1024 &&
              value->GetType("password")==VTYPE_STRING && value->GetString("password").length()<=4096;
      if(success) {
        auto password=value->GetString("password").ToString();
        tilo::CredentialSecret secret(password);
        volatile char* data=password.data();for(size_t i=0;i<password.size();++i)data[i]=0;
        const auto username=value->GetString("username").ToString();
        success=credential_consent_.Offer(std::to_string(id),*credential_identity_,pending.form,username,std::move(secret),tilo::CredentialDocument::Clock::now());
        if(success) {
          fields->SetString("capture",std::to_string(id));fields->SetString("username",username);
          CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self){
            self->credential_consent_.Expire(tilo::CredentialDocument::Clock::now());
          },CefRefPtr<Client>(this)),30000);
        }
      }
    } else if(success) {
      auto forms=value->GetList("forms");success=forms && forms->GetSize()<=32;
      auto safe=CefListValue::Create();
      if(success)for(size_t index=0;index<forms->GetSize();++index) {
        auto form=forms->GetDictionary(index);
        if(!form || form->GetType("id")!=VTYPE_STRING || !tilo::ValidCredentialCounter(form->GetString("id").ToString())){success=false;break;}
        auto entry=CefDictionaryValue::Create();entry->SetString("id",form->GetString("id"));
        if(pending.action=="discover") {
          if(form->GetType("username")!=VTYPE_STRING || form->GetString("username").ToString().size()>1024 ||
             form->GetType("kind")!=VTYPE_STRING || (form->GetString("kind")!="current-password" && form->GetString("kind")!="new-password")){success=false;break;}
          entry->SetString("username",form->GetString("username"));entry->SetString("kind",form->GetString("kind"));
        }
        safe->SetDictionary(safe->GetSize(),entry);
      }
      if(success)fields->SetList("forms",safe);
    }
    CredentialEvent(pending.request,pending.action,success,success?(pending.action=="capture"?"Choose Save or Update to store this login":"Ready"):"Form changed or is unsupported",fields);
  }
  void InvalidateCredentials() {
    CancelCredentialStoreRequests();credential_document_.Invalidate();credential_handshake_.Invalidate();
    credential_consent_.Clear();credential_pending_.clear();credential_confirming_.clear();
    credential_identity_.reset();credential_committed_origin_.clear();
    if(credential_generation_<9007199254740991ULL) ++credential_generation_;
    else credential_generation_exhausted_=true;
  }
  bool CredentialPageSecure(CefRefPtr<CefFrame> frame) const {
    if(private_mode || !visible_ || transfer_owner_ || native_closing_ || !browser_ || !frame || !frame->IsValid() ||
       !frame->IsMain() || credential_generation_exhausted_) return false;
    if(credential_identity_ && (credential_identity_->browser_id!=browser_->GetIdentifier() ||
       credential_identity_->frame!=frame->GetIdentifier().ToString()))return false;
    const auto origin=tilo::Origin(frame->GetURL());
    if(!origin.starts_with("https://") || origin!=credential_committed_origin_) return false;
    auto entry=browser_->GetHost()->GetVisibleNavigationEntry();
    if(!entry || tilo::Origin(entry->GetURL())!=origin) return false;
    auto ssl=entry->GetSSLStatus();
    return ssl && ssl->IsSecureConnection() && !CefIsCertStatusError(ssl->GetCertStatus()) &&
           ssl->GetContentStatus()==0;
  }
  void OfferCredentialContext(CefRefPtr<CefFrame> frame) {
    if(!CredentialPageSecure(frame)) {TraceGeometry("credential-offer-ineligible",id_);return;}
    TraceGeometry("credential-offer",id_);
    credential_handshake_.Offer({browser_->GetIdentifier(),frame->GetIdentifier().ToString(),
                                tilo::Origin(frame->GetURL()),{},credential_generation_});
    auto message=CefProcessMessage::Create("tilo.credential_probe");
    message->GetArgumentList()->SetString(0,std::to_string(credential_generation_));
    message->GetArgumentList()->SetString(1,frame->GetURL());
    frame->SendProcessMessage(PID_RENDERER,message);
  }
  int id_;bool visible_=true,transfer_owner_=false,native_closing_=false;int key_modifiers_=0;uint64_t favicon_generation_=0;
  std::map<std::string,std::pair<uint64_t,bool>> media_frames_;
  std::optional<uint64_t> pending_main_media_token_;
  std::optional<bool> last_media_playing_;bool media_limit_reported_=false;
  tilo::FullscreenState fullscreen_;
  int audio_channels_=2;
  tilo::CredentialDocument credential_document_;
  tilo::CredentialHandshake credential_handshake_;
  tilo::CredentialConsent credential_consent_;
  std::map<uint64_t,CredentialPending> credential_pending_;
  std::map<uint64_t,tilo::CapturedCredential> credential_confirming_;
  std::map<uint64_t,CredentialStorePending> credential_store_pending_;
  std::optional<tilo::CredentialIdentity> credential_identity_;
  uint64_t credential_generation_=0;
  bool credential_generation_exhausted_=false;
  std::string credential_committed_origin_;
  bool main_navigation_finished_=false;
  bool bootstrap_settled_=false,navigation_busy_=true,navigation_dispatched_=false,navigation_started_=false,navigation_aborting_=false;
  std::string navigation_target_;
  std::optional<int> navigation_owner_;
  std::optional<std::string> navigation_error_;
  std::set<std::string> navigation_urls_;
  std::optional<std::string> failed_main_navigation_;
  CefRefPtr<tilo::NavigationHistory> navigation_history_;
  CefRefPtr<tilo::PageAutomation> page_automation_;
  CefRefPtr<tilo::AgentLog> agent_log_;
  CefRefPtr<tilo::PaintSnapshot> paint_snapshot_;
  CefRefPtr<tilo::Capabilities> capabilities_;
  CefRefPtr<tilo::StartupDiagnostics> startup_diagnostics_;
  CefRefPtr<tilo::DocumentScripts> document_scripts_,composition_scripts_;std::vector<CefRefPtr<CefDictionaryValue>> pending_navigation_;
  CefRefPtr<tilo::SiteClear> site_clear_;
  int pixel_width_=0,pixel_height_=0,popup_width_=0,popup_height_=0,mouse_modifiers_=0;
  CefRefPtr<CefRunContextMenuCallback> context_callback_;int context_id_=0;std::set<int> context_commands_;
  bool finding_=false,select_initial_=false;CefString find_text_;int find_id_=0;std::deque<CefRefPtr<CefDictionaryValue>> pending_find_;
  tilo::ViewGeometry geometry_;bool stale_paint_recovery_pending_=false;bool popup_visible_=false;CefRect popup_rect_;std::string pixels_,popup_;std::vector<CefString> scripts_,blocking_scripts_;
  IMPLEMENT_REFCOUNTING(Client);
};
void FaviconDownload::OnDownloadImageFinished(const CefString&,int,CefRefPtr<CefImage> image) {
  auto found=clients.find(tab_);if(found!=clients.end()) found->second->PublishFavicon(generation_,page_,image);
}
void PublishFilters() {for(auto& entry:clients)entry.second->RefreshFilters();}
void PublishZoomLevels() { for(auto& entry:clients) entry.second->PublishZoom(); }
size_t DetachedOwners() {return std::count_if(clients.begin(),clients.end(),[](const auto& entry){return entry.second->detached();});}
size_t VisibleOwners() {return std::count_if(clients.begin(),clients.end(),[](const auto& entry){return !entry.second->detached();});}
void PublishDownloadRuntime() {
  if(!downloads)return;auto d=CefDictionaryValue::Create();d->SetInt("admitted",static_cast<int>(downloads->Admitted()));d->SetInt("detached",static_cast<int>(DetachedOwners()));d->SetBool("accepting",downloads->Accepting());d->SetString("error","");Event("download_runtime",d);
}
void DownloadOwnerIdle(int id) {
  auto found=clients.find(id);
  const bool detached=found!=clients.end() && found->second->detached();
  const bool busy=downloads && downloads->OwnerBusy(id);
  if(found!=clients.end() && tilo::ShouldCloseDetached(detached,busy))found->second->CloseNative();
  PublishDownloadRuntime();
}
void CreateTab(int id) {
  if(closing || id<=0 || clients.count(id) || VisibleOwners()>=32 || clients.size()>=64) {auto d=CefDictionaryValue::Create();d->SetInt("tab",id);Event("closed",d);return;}
  auto client=CefRefPtr<Client>(new Client(id));clients.emplace(id,client);
  CefWindowInfo window;window.SetAsWindowless(0);CefBrowserSettings settings;settings.windowless_frame_rate=kOffscreenFrameRate;settings.background_color=CefColorSetARGB(255,255,255,255);
  TraceGeometry("create-begin",id);
  if(!CefBrowserHost::CreateBrowserSync(window,client,"about:blank",settings,nullptr,shared_context)) {clients.erase(id);CloseHost();}
  TraceGeometry("create-return",id);
}
void Dispatch(CefRefPtr<CefDictionaryValue> d) {
  const auto op=d->GetString("op");int id=d->HasKey("tab")?d->GetInt("tab"):1;
  if(op=="shutdown") {CloseHost();return;}
  if(op=="create_tab") {CreateTab(id);return;}
  if(op=="devtools_call") {DevToolsCall(d,id);return;}
  if(op=="download_start") {
    auto found=clients.find(id);
    if(downloads->Accepting() && found!=clients.end() && found->second->accepts_downloads())
      found->second->browser_->GetHost()->StartDownload(d->GetString("url"));
    return;
  }
  if(op=="download_decision") { downloads->DownloadDecision(d->GetInt("id"),d->GetBool("allow"));return; }
  if(op=="download_cancel") { downloads->CancelDownload(d->GetInt("id"));return; }
  if(op=="download_cancel_all") {downloads->CancelAll();return;}
  if(op=="download_retry") {auto found=clients.find(id);if(found!=clients.end() && found->second->accepts_downloads())downloads->Retry(d->GetInt("id"),found->second->browser_);return;}
  if(op=="resize") TraceGeometry("resize-dispatch",id,d->GetInt("width"),d->GetInt("height"),Number(d,"scale_factor"));
  auto found=clients.find(id);if(found!=clients.end()) found->second->Command(d);
}
#if defined(OS_LINUX)
// Whether this machine has a GPU render node the browser could use.
bool HasRenderNode() {
  for(int node=128;node<136;++node)
    if(access(("/dev/dri/renderD"+std::to_string(node)).c_str(),R_OK|W_OK)==0) return true;
  return false;
}
#endif
// Add `feature` to a comma-separated feature switch, keeping any already set.
void AddFeature(CefRefPtr<CefCommandLine> command_line,const char* name,const std::string& feature) {
  std::string value=command_line->HasSwitch(name)?command_line->GetSwitchValue(name).ToString():std::string();
  if((","+value+",").find(","+feature+",")!=std::string::npos) return;
  command_line->AppendSwitchWithValue(name,value.empty()?feature:value+","+feature);
}
class App final : public CefApp,public CefBrowserProcessHandler {
 public:
  CefRefPtr<CefBrowserProcessHandler> GetBrowserProcessHandler() override { return this; }
  void OnBeforeCommandLineProcessing(const CefString& process_type,CefRefPtr<CefCommandLine> command_line) override {
    // The Rust application owns browser onboarding; Chromium's first-run UI
    // cannot be completed in an offscreen embedding and can stall Linux init.
    if(process_type.empty()) command_line->AppendSwitch("no-first-run");
    // Chromium serves the Extensions domain on the pipe the host created;
    // `unsafe-extension-debugging` is what enables its loadUnpacked.
    if(process_type.empty() && devtools_write>=0) {
      command_line->AppendSwitch("remote-debugging-pipe");
      command_line->AppendSwitch("enable-unsafe-extension-debugging");
    }
    // The process budget: the browser and its storage service, then one
    // renderer per site. Tabs of one site share a renderer while different
    // sites stay isolated; no spare renderer waits for a tab that may never
    // open; the network service and GPU work run inside the browser process
    // (a product decision: fewer processes over their separate sandboxes).
    if(process_type.empty()) {
      for(const char* flag:{"process-per-site","in-process-gpu"})
        if(!command_line->HasSwitch(flag)) command_line->AppendSwitch(flag);
      AddFeature(command_line,"enable-features","NetworkServiceInProcess2");
      AddFeature(command_line,"disable-features","SpareRendererForSitePerProcess");
    }
#if defined(OS_LINUX)
    // Every frame is rendered offscreen, yet Chromium's default Linux platform
    // exits unless it reaches an X server, and a Tilo app sandbox has none.
    if(process_type.empty() && !std::getenv("DISPLAY") && !command_line->HasSwitch("ozone-platform"))
      command_line->AppendSwitchWithValue("ozone-platform","headless");
    // Without a GPU (a hosting server running agents), in-process GPU work
    // fails EGL and takes the browser down: compose in software instead, and
    // trade caches for memory as Chromium does on low-end devices.
    if(process_type.empty() && !HasRenderNode()) {
      for(const char* flag:{"disable-gpu","enable-low-end-device-mode"})
        if(!command_line->HasSwitch(flag)) command_line->AppendSwitch(flag);
    }
#endif
  }
  void OnContextInitialized() override {
    request_filter=new tilo::RequestFilter([] { StringEvent("error","message","Native request matcher failed to check a resource"); });
    if(private_mode) {
      CefRequestContextSettings settings;
      // Empty cache_path is CEF's incognito request context. The separate
      // root_cache_path below still gives CEF an owned installation-data root.
      shared_context=CefRequestContext::CreateContext(settings,request_filter);
    } else {
      shared_context=CefRequestContext::CreateContext(CefRequestContext::GetGlobalContext(),request_filter);
    }
    downloads=new tilo::Downloads(
      downloads_directory,
      [](const std::string& name,CefRefPtr<CefDictionaryValue> fields){::Event(name,fields);},
      [](CefRefPtr<CefBrowser> browser){for(const auto& entry:clients)if(entry.second->browser_ && browser && entry.second->browser_->IsSame(browser))return entry.first;return 0;},
      [](int id){auto found=clients.find(id);return found!=clients.end() && found->second->accepts_downloads();},
      [](int id){auto found=clients.find(id);return found!=clients.end() && found->second->accepts_downloads() && download_owners.CanAdmit(id);},
      [](int id,bool busy){download_owners.SetBusy(id,busy);PublishDownloadRuntime();},
      [](int id){DownloadOwnerIdle(id);},
      []{PublishDownloadRuntime();});
    CreateTab(1);PublishDownloadRuntime();if(clients.empty()) CefQuitMessageLoop();
  }
 private: IMPLEMENT_REFCOUNTING(App);
};
bool ValidCommand(CefRefPtr<CefDictionaryValue> d) {
  if(d->HasKey("tab") && (d->GetType("tab")!=VTYPE_INT || d->GetInt("tab")<=0)) return false;
  auto op=d->GetString("op").ToString();
  auto type=[&](const char* key,cef_value_type_t expected){return d->GetType(key)==expected;};
  auto number=[&](const char* key){auto t=d->GetType(key);return t==VTYPE_INT || (t==VTYPE_DOUBLE && std::isfinite(d->GetDouble(key)));};
  auto no_payload=[&]{CefDictionaryValue::KeyList keys;d->GetKeys(keys);return std::all_of(keys.begin(),keys.end(),[](const CefString& key){return key=="op" || key=="tab";});};
  if(op=="create_tab" || op=="close_tab" || op=="try_close_tab") return d->HasKey("tab");
  if(op=="visibility") return type("visible",VTYPE_BOOL);
  if(op=="audio_muted") return type("muted",VTYPE_BOOL);
  if(op=="navigation_history" || op=="history_entry") return type("request_id",VTYPE_INT) && d->GetInt("request_id")>0 && (op=="navigation_history" ? (!d->HasKey("direction") || (type("direction",VTYPE_STRING) && (d->GetString("direction")=="back" || d->GetString("direction")=="forward"))) : (type("id",VTYPE_INT) && d->GetInt("id")>0));
  if(op=="scoped_navigation" || op=="cancel_navigation") {
    if(!type("request_id",VTYPE_INT) || d->GetInt("request_id")<=0)return false;
    const auto command=d->GetString("command").ToString();
    if(op=="scoped_navigation" && (!type("command",VTYPE_STRING) || (command!="navigate" && command!="back" && command!="forward" && command!="reload" && command!="history_entry")))return false;
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="request_id" && !(op=="scoped_navigation" && (key=="command" || (command=="navigate" && key=="url") || (command=="history_entry" && (key=="history_request_id" || key=="id")))))return false;
    if(command=="navigate")return type("url",VTYPE_STRING) && !d->GetString("url").empty() && d->GetString("url").length()<=8192;
    if(command=="history_entry")return type("history_request_id",VTYPE_INT) && d->GetInt("history_request_id")>0 && type("id",VTYPE_INT) && d->GetInt("id")>0;
    return true;
  }
  if(op=="navigate") return type("url",VTYPE_STRING) && !d->GetString("url").empty();
  if(op=="download_start") {
    if(!type("url",VTYPE_STRING))return false;
    CefURLParts parts;
    if(!CefParseURL(d->GetString("url"),parts))return false;
    const auto scheme=CefString(&parts.scheme).ToString();
    return (scheme=="https" || scheme=="http") && !CefString(&parts.host).empty();
  }
  if(op=="clear_on_exit") return type("enabled",VTYPE_BOOL);
  if(op=="permission_decision" || op=="download_decision") return type("id",VTYPE_INT) && d->GetInt("id")>0 && type("allow",VTYPE_BOOL);
  if(op=="context_action") return type("id",VTYPE_INT) && d->GetInt("id")>0 && type("command",VTYPE_INT) && d->GetInt("command")>=0;
  if(op=="find") return type("id",VTYPE_INT) && d->GetInt("id")>0 && type("text",VTYPE_STRING) && d->GetString("text").length()<=4096 && type("forward",VTYPE_BOOL) && type("next",VTYPE_BOOL);
  if(op=="stop_find") return true;
  if(op=="exit_fullscreen") return no_payload();
  if(op=="zoom") return number("level") && Number(d,"level")>=-5 && Number(d,"level")<=9;
  if(op=="download_cancel" || op=="download_retry") return type("id",VTYPE_INT) && d->GetInt("id")>0;
  if(op=="modifiers") return type("ctrl",VTYPE_BOOL) && type("shift",VTYPE_BOOL) && type("alt",VTYPE_BOOL) && type("meta",VTYPE_BOOL);
  if(op=="focus") return type("focused",VTYPE_BOOL);
  if(op=="resize") return type("width",VTYPE_INT) && type("height",VTYPE_INT) && number("scale_factor");
  if(op=="mouse_move" || op=="mouse_button" || op=="scroll") {
    if(!number("x") || !number("y") || std::abs(Number(d,"x"))>1000000 || std::abs(Number(d,"y"))>1000000) return false;
    if(op=="scroll") return number("dx") && number("dy") && std::abs(Number(d,"dx"))<=1000000 && std::abs(Number(d,"dy"))<=1000000;
    if(op=="mouse_button") { auto b=d->GetString("button");return type("down",VTYPE_BOOL) && (b=="left" || b=="middle" || b=="right"); }
    return true;
  }
  if(op=="edit_history") {
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="direction")return false;
    return type("direction",VTYPE_STRING) &&
      (d->GetString("direction")=="undo" || d->GetString("direction")=="redo");
  }
  if(op=="key") return type("key",VTYPE_STRING) && type("down",VTYPE_BOOL);
  if(op=="scripts" || op=="blocking_scripts") {
    if(!type("scripts",VTYPE_LIST)) return false;
    auto list=d->GetList("scripts");for(size_t i=0;i<list->GetSize();++i) if(list->GetType(i)!=VTYPE_STRING) return false;
    return true;
  }
  if(op=="site_exceptions") {if(!type("origins",VTYPE_LIST))return false;auto list=d->GetList("origins");if(list->GetSize()>1024)return false;for(size_t i=0;i<list->GetSize();++i){if(list->GetType(i)!=VTYPE_STRING)return false;const auto origin=list->GetString(i).ToString();if(origin.empty() || tilo::Origin(origin)!=origin)return false;}return true;}
  if(op=="credential_action") {
    if(!type("request_id",VTYPE_INT) || d->GetInt("request_id")<=0 || !type("action",VTYPE_STRING) ||
       !type("origin",VTYPE_STRING) || d->GetString("origin").ToString().size()>8192 ||
       !type("document",VTYPE_STRING))return false;
    const auto action=d->GetString("action").ToString();
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);std::vector<std::string> fields;
    for(const auto& key:keys)fields.push_back(key.ToString());
    if(!tilo::CredentialFieldsValid(action,fields))return false;
    if(action!="status" && action!="cancel" && !tilo::ValidCredentialCounter(d->GetString("document").ToString()))return false;
    if(d->HasKey("account") && (!type("account",VTYPE_STRING) || d->GetString("account").empty() || d->GetString("account").ToString().size()>128))return false;
    for(const auto* key:{"capture","form"})if(d->HasKey(key) && (!type(key,VTYPE_STRING) || !tilo::ValidCredentialCounter(d->GetString(key).ToString())))return false;
    if(action=="fill_address") {
      if(!type("profile",VTYPE_DICTIONARY))return false;
      auto profile=d->GetDictionary("profile");CefDictionaryValue::KeyList names;profile->GetKeys(names);
      if(names.empty() || names.size()>8)return false;
      for(const auto& name:names) {
        const auto key=name.ToString();
        if((key!="full_name" && key!="given_name" && key!="family_name" && key!="street" && key!="city" && key!="region" && key!="postal_code" && key!="country") ||
           profile->GetType(name)!=VTYPE_STRING || profile->GetString(name).ToString().size()>1024)return false;
      }
    }
    return true;
  }
  if(op=="site_status" || op=="site_action") {
    if(!type("request_id",VTYPE_INT) || d->GetInt("request_id")<=0 || !type("origin",VTYPE_STRING) || d->GetString("origin").length()>8192)return false;
    if(op=="site_status")return true;
    const auto action=d->GetString("action");return number("generation") && tilo::ValidDocumentGeneration(Number(d,"generation")) && (action=="clear" || action=="revoke" || action=="enable" || action=="disable" || action=="reload");
  }
  if(op=="text_input") {
    if(!type("request_id",VTYPE_INT) || d->GetInt("request_id")<=0 || !type("action",VTYPE_STRING))return false;
    const auto action=d->GetString("action").ToString();
    if(action!="prepare_text_input" && action!="start_text_input" && action!="check_text_input" && action!="commit_text_input" && action!="cancel_text_input" && action!="undo_text_input" && action!="redo_text_input")return false;
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="request_id" && key!="action" && key!="lease" && key!="text" && key!="expires_ms" && key!="history_scope" && key!="native_caret")return false;
    if(d->HasKey("native_caret") && (!type("native_caret",VTYPE_BOOL) || action!="prepare_text_input"))return false;
    if(action!="prepare_text_input") {
      if(!type("lease",VTYPE_STRING))return false;
      const auto lease=d->GetString("lease").ToString();
      if(lease.size()!=32 || !std::all_of(lease.begin(),lease.end(),[](char c){return (c>='0' && c<='9') || (c>='a' && c<='f');}))return false;
    }
    if(d->HasKey("history_scope") && (!type("history_scope",VTYPE_STRING) || (action!="undo_text_input" && action!="redo_text_input") || d->GetString("history_scope")!="page"))return false;
    if(d->HasKey("expires_ms") && (!number("expires_ms") || Number(d,"expires_ms")<=0 || !std::isfinite(Number(d,"expires_ms"))))return false;
    return action!="commit_text_input" || (type("text",VTYPE_STRING) && !d->GetString("text").empty() && d->GetString("text").ToString().size()<=4096);
  }
  if(op=="page") {
    if(!type("request_id",VTYPE_INT) || d->GetInt("request_id")<=0 || !type("action",VTYPE_STRING))return false;
    const auto action=d->GetString("action").ToString();
    if(action!="outline" && action!="click" && action!="fill" && action!="select" && action!="press" && action!="submit" &&
       action!="changes" && action!="at" && action!="settle" && action!="console" && action!="network")return false;
    const bool elementless=action=="outline" || action=="press" || action=="changes" || action=="at" || action=="settle" ||
      action=="console" || action=="network";
    if(d->HasKey("element") ? !type("element",VTYPE_STRING) || d->GetString("element").empty() || d->GetString("element").length()>64
                            : !elementless)return false;
    for(const char* flag:{"cross_origin","secret"})if(d->HasKey(flag) && !type(flag,VTYPE_BOOL))return false;
    if(action=="at" && (!number("x") || !number("y") || Number(d,"x")<0 || Number(d,"y")<0 || Number(d,"x")>100000 || Number(d,"y")>100000))return false;
    if(action=="settle" && (!type("quiet_ms",VTYPE_INT) || !type("timeout_ms",VTYPE_INT) || d->GetInt("quiet_ms")<50 ||
       d->GetInt("timeout_ms")<d->GetInt("quiet_ms") || d->GetInt("timeout_ms")>20000))return false;
    if((action=="console" || action=="network") && (!number("since") || Number(d,"since")<0))return false;
    if(action=="fill" && (!type("text",VTYPE_STRING) || d->GetString("text").length()>65536))return false;
    if(action=="select") {
      if(!type("values",VTYPE_LIST))return false;auto values=d->GetList("values");
      if(!values->GetSize() || values->GetSize()>64)return false;
      for(size_t i=0;i<values->GetSize();++i)if(values->GetType(i)!=VTYPE_STRING || values->GetString(i).length()>1024)return false;
    }
    if(action=="press") {
      if(!type("key",VTYPE_STRING) || d->GetString("key").empty() || d->GetString("key").length()>16)return false;
      for(const char* flag:{"ctrl","shift","alt","meta"})if(!type(flag,VTYPE_BOOL))return false;
    }
    return true;
  }
  if(op=="paint_capture") {
    if(!type("request_id",VTYPE_INT) || d->GetInt("request_id")<=0 || !type("styles",VTYPE_LIST))return false;
    auto styles=d->GetList("styles");if(!styles->GetSize() || styles->GetSize()>64)return false;
    for(size_t i=0;i<styles->GetSize();++i)if(styles->GetType(i)!=VTYPE_STRING || styles->GetString(i).empty() || styles->GetString(i).length()>64)return false;
    return true;
  }
  if(op=="paint_resource") return type("request_id",VTYPE_INT) && d->GetInt("request_id")>0 && type("url",VTYPE_STRING) && !d->GetString("url").empty() && d->GetString("url").length()<=1048576;
  if(op=="devtools_call") {
    if(!type("request_id",VTYPE_INT) || d->GetInt("request_id")<=0 || !type("method",VTYPE_STRING) || !tilo::DevToolsMethodAllowed(d->GetString("method").ToString()))return false;
    if(d->HasKey("params") && !type("params",VTYPE_DICTIONARY))return false;
    if(d->HasKey("session") && (!type("session",VTYPE_STRING) || d->GetString("session").length()>128))return false;
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="request_id" && key!="method" && key!="params" && key!="session")return false;
    return true;
  }
  if(op=="filter_rules") return type("rules",VTYPE_STRING) && (type("enabled",VTYPE_BOOL) || type("enabled",VTYPE_NULL));
  return op=="back" || op=="forward" || op=="reload" || op=="stop" || op=="clear_data" || op=="devtools" || op=="download_cancel_all" || op=="shutdown";
}
void DevToolsReply(int tab,int request_id,bool ok,const std::string& body) {
  auto d=CefDictionaryValue::Create();d->SetInt("tab",tab);d->SetInt("request_id",request_id);d->SetBool("ok",ok);d->SetString("body",body);Event("devtools_result",d);
}
// Forward one allow-listed DevTools command; the reply arrives as a
// `devtools_result` event for the same tab and request.
void DevToolsCall(CefRefPtr<CefDictionaryValue> d,int tab) {
  const int request_id=d->GetInt("request_id");
  if(devtools_write<0) {DevToolsReply(tab,request_id,false,"Extension management is unavailable");return;}
  auto id=devtools_pending.Add(tab,request_id);
  if(!id) {DevToolsReply(tab,request_id,false,"Too many extension requests are pending");return;}
  auto message=CefDictionaryValue::Create();
  message->SetInt("id",*id);message->SetString("method",d->GetString("method"));
  if(d->HasKey("params")) message->SetDictionary("params",d->GetDictionary("params")->Copy(true));
  if(d->HasKey("session")) message->SetString("sessionId",d->GetString("session"));
  auto value=CefValue::Create();value->SetDictionary(message);
  std::string text=CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString();
  if(text.size()>tilo::kMaxDevToolsMessage) {devtools_pending.Forget(*id);DevToolsReply(tab,request_id,false,"Extension request is too large");return;}
  text.push_back('\0');
  // The reply thread never blocks this write: Chromium drains the pipe.
  size_t done=0;
  while(done<text.size()) {
    ssize_t n=write(devtools_write,text.data()+done,text.size()-done);
    if(n<0 && errno==EINTR) continue;
    if(n<=0) {devtools_pending.Forget(*id);DevToolsReply(tab,request_id,false,"Extension management is unavailable");return;}
    done+=size_t(n);
  }
}
void ReadDevToolsReplies(std::atomic<bool>& stop) {
  std::string buffer;char chunk[65536];
  while(!stop) {
    pollfd fd{devtools_read,POLLIN,0};int r=poll(&fd,1,100);
    if(r<0 && errno==EINTR) continue;if(r<0) break;if(!r) continue;
    ssize_t n=read(devtools_read,chunk,sizeof(chunk));if(n<=0) break;buffer.append(chunk,size_t(n));
    std::string messages[16];size_t count=0;
    do {
      if(!tilo::TakeDevToolsMessages(buffer,messages,16,&count)) return;
      for(size_t i=0;i<count;++i) {
        auto parsed=CefParseJSON(messages[i],JSON_PARSER_RFC);
        if(!parsed || parsed->GetType()!=VTYPE_DICTIONARY) continue;
        auto reply=parsed->GetDictionary();
        // Notifications carry no id; only commands this host sent are answered.
        if(reply->GetType("id")!=VTYPE_INT) continue;
        auto owner=devtools_pending.Take(reply->GetInt("id"));if(!owner) continue;
        const bool ok=reply->HasKey("result");
        auto payload=ok ? reply->GetValue("result") : reply->GetValue("error");
        DevToolsReply(owner->tab,owner->request_id,ok,payload ? CefWriteJSON(payload,JSON_WRITER_DEFAULT).ToString() : std::string("{}"));
      }
    } while(count==16);
  }
}
void ReadCommands(std::atomic<bool>& stop) {
  std::string buffer;char chunk[65536];
  while(!stop && !output->Failed()) {
    pollfd fd{STDIN_FILENO,POLLIN,0};int r=poll(&fd,1,100);
    if(r<0 && errno==EINTR) continue;if(r<0) break;if(!r) continue;
    ssize_t n=read(STDIN_FILENO,chunk,sizeof(chunk));if(n<=0) break;buffer.append(chunk,n);
    bool bad=false;
    while(buffer.size()>=4) {
      auto size=tilo::U32(buffer.data());if(!size || size>tilo::kMaxPacket) {bad=true;break;}
      if(buffer.size()<size_t(size)+4) break;
      if(buffer[4]!=1) {bad=true;break;}
      auto parsed=CefParseJSON(buffer.substr(5,size-1),JSON_PARSER_RFC);
      if(!parsed || parsed->GetType()!=VTYPE_DICTIONARY || parsed->GetDictionary()->GetType("op")!=VTYPE_STRING) {bad=true;break;}
      auto d=parsed->GetDictionary();if(!ValidCommand(d)) {bad=true;break;}
      if(d->GetString("op")=="resize") TraceGeometry("resize-received",d->HasKey("tab")?d->GetInt("tab"):1,d->GetInt("width"),d->GetInt("height"),Number(d,"scale_factor"));
      CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<CefDictionaryValue> command){Dispatch(command);},d));
      buffer.erase(0,size+4);
    }
    if(bad) { std::cerr<<"Malformed browser IPC packet\n";break; }
  }
  if(!stop) CefPostTask(TID_UI,base::BindOnce(&CloseHost));
}
}
CefRefPtr<CefApp> MakeHostApp() { return new App; }
void CloseHost() {
  TraceGeometry("close-host",0);
  closing=true;
  if(downloads) downloads->Close();
  if(clients.empty()) {CefQuitMessageLoop();return;}
  const auto live=clients;for(const auto& [id,client]:live) if(client->browser_) {client->browser_->GetHost()->CloseDevTools();client->browser_->GetHost()->CloseBrowser(true);}
}
int RunHost(int argc,char** argv) {
  signal(SIGPIPE,SIG_IGN);
  std::string profile;
  for(int i=1;i<argc;++i) { std::string arg=argv[i];if(arg.rfind("--profile=",0)==0) profile=arg.substr(10);if(arg.rfind("--downloads=",0)==0) downloads_directory=arg.substr(12);if(arg=="--private") private_mode=true;if(arg=="--tilo-audio-bridge") audio_bridge=true; }
  if(profile.empty() || profile[0]!='/') {std::cerr<<"--profile requires an absolute directory\n";return 2;}
  std::filesystem::create_directories(profile);
  profile=std::filesystem::canonical(profile).string();
  if(downloads_directory.empty()) downloads_directory=profile+"/downloads";
  Output writer;output=&writer;
#if defined(OS_LINUX)
  // Chromium's pipe handler reads commands on fd 3 and replies on fd 4. Use
  // them only when nothing else holds those descriptors.
  if(fcntl(3,F_GETFD)<0 && fcntl(4,F_GETFD)<0) {
    // Keep every pipe end clear of 3 and 4 until it is moved there on purpose.
    auto high=[](int fd){int moved=fcntl(fd,F_DUPFD_CLOEXEC,5);close(fd);return moved;};
    int commands[2],replies[2];
    if(pipe2(commands,O_CLOEXEC)==0 && pipe2(replies,O_CLOEXEC)==0) {
      int chromium_reads=high(commands[0]),host_writes=high(commands[1]);
      int host_reads=high(replies[0]),chromium_writes=high(replies[1]);
      if(chromium_reads>=0 && host_writes>=0 && host_reads>=0 && chromium_writes>=0 &&
         dup2(chromium_reads,3)==3 && dup2(chromium_writes,4)==4) {
        devtools_write=host_writes;devtools_read=host_reads;
      } else {
        for(int fd:{host_writes,host_reads})if(fd>=0)close(fd);
      }
      for(int fd:{chromium_reads,chromium_writes})if(fd>=0)close(fd);
    }
  }
#endif
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
#if defined(OS_MAC)
  if(!private_mode) {
    auto store=tilo::MakeMacCredentialStore(tilo::CredentialServiceForProfile(profile));
    if(store)credential_worker=std::make_shared<tilo::CredentialWorker>(false,std::move(store));
  }
#endif
  {std::lock_guard lock(credential_delivery->mutex);credential_delivery->active=true;}
  std::atomic<bool> stop{false};std::thread reader([&]{ReadCommands(stop);});
  std::thread devtools_reader;if(devtools_read>=0) devtools_reader=std::thread([&]{ReadDevToolsReplies(stop);});
  CefRunMessageLoop();TraceGeometry("message-loop-return",0);stop=true;reader.join();if(devtools_reader.joinable())devtools_reader.join();TraceGeometry("reader-joined",0);
  {std::lock_guard lock(credential_delivery->mutex);credential_delivery->active=false;}
  if(credential_worker)credential_worker->Stop();credential_worker.reset();clients.clear();shared_context=nullptr;request_filter=nullptr;TraceGeometry("cef-shutdown-begin",0);CefShutdown();TraceGeometry("cef-shutdown-end",0);writer.Stop();output=nullptr;
  if(clear_on_exit) { std::error_code ec;std::filesystem::remove_all(profile+"/default",ec);if(ec) {std::cerr<<"Could not clear Chromium profile: "<<ec.message()<<"\n";return 1;} }
  return writer.Failed()?1:0;
}
