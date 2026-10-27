#include "host.h"
#include "protocol.h"
#include "frame_memory.h"
#include "view_geometry.h"
#include "key_mapping.h"
#include "request_filter.h"
#include "document_scripts.h"
#include "startup_diagnostics.h"
#include "capabilities.h"
#include "site_info.h"
#include "navigation_history.h"
#include "include/cef_drag_handler.h"
#include "page_automation.h"
#include "geolocation.h"
#include "agent_log.h"
#include "paint_snapshot.h"
#include "document_size.h"
#include "fullscreen_state.h"
#include "audio_pcm.h"
#include "credential_document.h"
#include "credential_consent.h"
#include "credential_commands.h"
#include "service_sessions.h"
#include "devtools_bridge.h"
#include "extension_tabs.h"
#include "page_clipboard.h"
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
#include <deque>
#include <fcntl.h>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <iterator>
#include <mutex>
#include <map>
#include <memory>
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
// The widest picture the page op paints, matching the app's own bound: a
// wider request is a malformed command, never a larger answer.
constexpr int CAPTURE_MAX_WIDTH = 1600;
// Bounded, opt-in startup geometry evidence; never includes page content.
void TraceGeometry(const char* stage,int tab,int width=0,int height=0,double scale=0) {
  static const bool enabled=[] {const char* value=std::getenv("TILO_WEBVIEW_PROFILE");return value && std::strcmp(value,"trace")==0;}();
  static std::atomic<unsigned> count{0};
  if(enabled && count.fetch_add(1)<128) std::fprintf(stderr,"[chromium-geometry] %s tab=%d %dx%d scale=%g\n",stage,tab,width,height,scale);
}
// The app applies a frame's changed rects to the frame before it
// (`--tilo-frame-damage`), so a caret's blink is not the whole page again.
bool frame_damage=false;
// CEF callbacks only enqueue: a slow UI must never block Chromium's UI thread.
class Output {
 public:
  // With `memory`, a whole frame's pixels go into it and only a notice down
  // the pipe (frame_memory.h); without it, the whole frame goes down the
  // pipe. Changed rects (--tilo-frame-damage) go down the pipe either way.
  explicit Output(std::unique_ptr<tilo::FrameMemory> memory):memory_(std::move(memory)),thread_([this]{ Run(); }) {}
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
  void Send(uint8_t kind,const std::string& data) {
    auto packet=tilo::EncodePacket(kind,data);
    std::lock_guard<std::mutex> lock(mutex_);
    if(events_.size()<256) events_.push_back(std::move(packet));
    else failed_=true;
    cv_.notify_one();
  }
  // A frame packet (tilo::FramePacket), shared with the tab that keeps it,
  // with what changed since the tab's frame before it (`damage`; null when
  // not known): only each tab's newest pending frame is kept, so one tab's
  // paints never drop another's last frame, and it keeps the rects of the
  // frames it replaced. With --tilo-frame-damage the writer sends just those
  // rects (tilo::DamagePacket) when the app has the frame they change.
  void SendFrame(uint32_t tab,std::shared_ptr<const std::string> packet,uint32_t width,uint32_t height,const std::vector<tilo::DamageRect>* damage=nullptr) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto [entry,fresh]=frames_.try_emplace(tab);
    auto& pending=entry->second;
    bool whole=!damage || (!fresh && (pending.whole || pending.width!=width || pending.height!=height));
    if(whole || fresh) pending.damage.clear();
    if(!whole) {
      pending.damage.insert(pending.damage.end(),damage->begin(),damage->end());
      // A reader that falls behind gets the frame whole, not a growing list.
      if(pending.damage.size()>4*tilo::kMaxDamageRects) {whole=true;pending.damage.clear();}
    }
    pending.whole=whole;pending.packet=std::move(packet);pending.width=width;pending.height=height;
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
      std::string owned;std::shared_ptr<const std::string> frame;PendingFrame damaged;uint32_t damaged_tab=0;int slot=-1;
      { std::unique_lock<std::mutex> lock(mutex_);
        bool next=false;
        while(!next) {
          if(!events_.empty()) { owned=std::move(events_.front()); events_.pop_front(); next=true; }
          else if(!audio_.empty()) { owned=std::move(audio_.front().second); audio_.pop_front(); next=true; }
          else if(!frames_.empty()) {
            // Tabs take turns, so a tab painting every frame cannot starve another.
            auto turn=frames_.upper_bound(last_frame_tab_);if(turn==frames_.end())turn=frames_.begin();
            // Only rects of the frame the app was sent last apply to it.
            auto& sent=sent_[turn->first];
            const bool damage=frame_damage && !turn->second.whole && sent.first==turn->second.width && sent.second==turn->second.height;
            if(!damage && memory_ && (slot=memory_->FreeSlot(last_slot_))<0) {
              if(stopping_) break;
              // A whole frame waits for the app to finish copying out a
              // slot: that takes a copy's time, and no wakeup announces it,
              // so look again shortly.
              cv_.wait_for(lock,std::chrono::milliseconds(1));continue;
            }
            last_frame_tab_=turn->first;PendingFrame pending=std::move(turn->second);frames_.erase(turn);
            if(damage) {damaged=pending;damaged_tab=last_frame_tab_;}
            sent={pending.width,pending.height};frame=std::move(pending.packet);next=true;
          }
          else if(stopping_) break;
          else cv_.wait(lock);
        }
        if(!next) break;
      }
      if(frame && slot>=0) {
        last_slot_=static_cast<uint32_t>(slot);
        owned=memory_->Publish(last_slot_,last_frame_tab_,*frame);frame.reset();
        if(owned.empty()) continue;
      }
      if(damaged_tab) {
        const size_t offset=tilo::FramePixelOffset(damaged_tab);
        auto rects=tilo::ClipDamage(damaged.damage,damaged.width,damaged.height);
        if(!rects.empty() && frame->size()>offset) {
          owned=tilo::DamagePacket(damaged_tab,damaged.width,damaged.height,frame->data()+offset,frame->size()-offset,rects);
          if(!owned.empty()) frame.reset();
        }
      }
      const std::string& packet=frame?*frame:owned;
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
  struct PendingFrame { std::shared_ptr<const std::string> packet; uint32_t width=0,height=0; std::vector<tilo::DamageRect> damage; bool whole=true; };
  std::atomic<bool> stopping_{false},failed_{false};
  std::mutex mutex_; std::condition_variable cv_; std::deque<std::string> events_; std::deque<std::pair<bool,std::string>> audio_; std::map<uint32_t,PendingFrame> frames_;
  // The dimensions of the frame last written for each tab.
  std::map<uint32_t,std::pair<uint32_t,uint32_t>> sent_; uint32_t last_frame_tab_=0;
  std::unique_ptr<tilo::FrameMemory> memory_; uint32_t last_slot_=tilo::kFrameSlots-1; std::thread thread_;
};
Output* output=nullptr;
// Page audio goes to the app (`--tilo-audio-bridge`) because its sandbox has no audio device; without it Chromium plays on its own.
bool audio_bridge=false;
void Event(const std::string& name,CefRefPtr<CefDictionaryValue> fields=nullptr) {
  if(!fields) fields=CefDictionaryValue::Create();
  fields->SetString("event",name); auto value=CefValue::Create(); value->SetDictionary(fields);
  output->Send(1,CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString());
}
// A pointer shape by the CSS name Tilo's toolkit takes it in.
const char* CursorName(cef_cursor_type_t type) {
  switch(type) {
    case CT_HAND: return "pointer";
    case CT_IBEAM: return "text";
    case CT_VERTICALTEXT: return "vertical-text";
    case CT_CROSS: return "crosshair";
    case CT_WAIT: return "wait";
    case CT_PROGRESS: return "progress";
    case CT_HELP: return "help";
    case CT_MOVE: return "move";
    case CT_CELL: return "cell";
    case CT_CONTEXTMENU: return "context-menu";
    case CT_ALIAS: return "alias";
    case CT_COPY: return "copy";
    case CT_NODROP: return "no-drop";
    case CT_NOTALLOWED: return "not-allowed";
    case CT_GRAB: return "grab";
    case CT_GRABBING: return "grabbing";
    case CT_ZOOMIN: return "zoom-in";
    case CT_ZOOMOUT: return "zoom-out";
    case CT_NONE: return "none";
    case CT_EASTRESIZE: return "e-resize";
    case CT_WESTRESIZE: return "w-resize";
    case CT_NORTHRESIZE: return "n-resize";
    case CT_SOUTHRESIZE: return "s-resize";
    case CT_NORTHEASTRESIZE: return "ne-resize";
    case CT_NORTHWESTRESIZE: return "nw-resize";
    case CT_SOUTHEASTRESIZE: return "se-resize";
    case CT_SOUTHWESTRESIZE: return "sw-resize";
    case CT_EASTWESTRESIZE: return "ew-resize";
    case CT_NORTHSOUTHRESIZE: return "ns-resize";
    case CT_NORTHEASTSOUTHWESTRESIZE: return "nesw-resize";
    case CT_NORTHWESTSOUTHEASTRESIZE: return "nwse-resize";
    case CT_COLUMNRESIZE: return "col-resize";
    case CT_ROWRESIZE: return "row-resize";
    default: return "default";
  }
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
// A page's popups are tabs too, numbered apart from the tabs the app opens.
int next_popup=1<<30;
CefRefPtr<CefRequestContext> shared_context;
void CreateTab(int id);
void PublishZoomLevels();
void PublishFilters();
void PublishDownloadRuntime();
void DownloadOwnerIdle(int id);
void Dispatch(CefRefPtr<CefDictionaryValue> command);
void DevToolsCall(CefRefPtr<CefDictionaryValue> d,int tab);
bool WritePipe(std::string text);
void StartExtensionTabs();
bool closing=false;
bool clear_on_exit=false;
bool private_mode=false;
// The host shows sealed documents (a mail): its pages never navigate. A
// followed link becomes an open_tab request for the embedder; everything
// else that would leave the document is refused.
bool sealed_documents=false;
bool SealedDocumentUrl(const std::string& url) { return url.rfind("data:",0)==0 || url=="about:blank"; }
std::string service_session_id;
bool service_session_consumed=false;
// Extension management over Chromium's DevTools pipe (see devtools_bridge.h).
// The host's ends of the pipe; -1 when the pipe is not available.
int devtools_write=-1,devtools_read=-1;
tilo::DevToolsPending devtools_pending;
std::string downloads_directory;
// Where extensions' context menu items are kept (extension_menus.h).
std::string extension_menus_file;
std::shared_ptr<tilo::CredentialWorker> credential_worker;
struct CredentialDeliveryState {std::mutex mutex;bool active=false;};
// A blocked worker retains this plain state through shutdown, without touching
// destroyed globals or retaining CEF objects after CefShutdown.
const auto credential_delivery=std::make_shared<CredentialDeliveryState>();
CefRefPtr<tilo::RequestFilter> request_filter;
// Tabs and windows for extensions (extension_tabs.h), and the page tab the
// person used last: an extension's "current window" when it has none.
std::unique_ptr<tilo::ExtensionTabs> extension_tabs;
int last_focused_tab=0;
// The engine has no clipboard of its own (page_clipboard.h): set where
// Chromium runs with no display server.
bool page_clipboard=false;
tilo::ClipboardReads clipboard_reads;
void ClipboardAnswer(int request,std::optional<std::string> text);
// Host-originated calls on the DevTools pipe, answered on the UI thread.
std::map<int,tilo::ExtensionTabs::PipeDone> pipe_calls;
int next_pipe_call=0;
// The contexts a session the host attached on the pipe reports, by session:
// only Runtime.executionContextCreated, and only for sessions listened to.
std::map<std::string,tilo::ExtensionTabs::PipeListener> pipe_listeners;
void PipeCall(const std::string& method,CefRefPtr<CefDictionaryValue> params,const std::string& session,tilo::ExtensionTabs::PipeDone done);
CefRefPtr<tilo::Downloads> downloads;
tilo::DownloadOwnerReservations download_owners;

class Client final : public CefClient, public CefRenderHandler, public CefLifeSpanHandler,
                     public CefDisplayHandler, public CefLoadHandler, public CefFrameHandler, public CefRequestHandler, public CefFindHandler, public CefContextMenuHandler, public CefDialogHandler, public CefJSDialogHandler, public CefAudioHandler, public CefDragHandler {
 public:
  explicit Client(int id):id_(id),credential_document_(private_mode),capabilities_(new tilo::Capabilities([this](const std::string& name,CefRefPtr<CefDictionaryValue> fields){Event(name,fields);})) {}
  // A page request the engine refuses before the page sees it is answered by
  // this tab's page automation, behind whatever it is still running.
  void RefusePage(CefRefPtr<CefDictionaryValue> d,const std::string& message) {
    // A refusal is judged before the tab's dispatch of the same request, so
    // the request is taken as never having arrived: the answer is the one the
    // word it was refused by says.
    if(page_automation_) { page_automation_->Refuse(d,"invalid_request",message);return; }
    auto fields=CefDictionaryValue::Create();
    fields->SetInt("request_id",d->GetInt("request_id"));fields->SetBool("ok",false);
    fields->SetString("code","invalid_request");fields->SetString("message",message);
    fields->SetString("navigation","none");fields->SetBool("navigated",false);fields->SetBool("loading",false);
    fields->SetNull("outline");
    Event("page_result",fields);
  }
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
  bool OnOpenURLFromTab(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,const CefString& url,cef_window_open_disposition_t disposition,bool) override {
    if(disposition==CEF_WOD_NEW_BACKGROUND_TAB || disposition==CEF_WOD_NEW_FOREGROUND_TAB || disposition==CEF_WOD_NEW_WINDOW) {
      if(extension_tabs && url.length()<=8192)extension_tabs->Opening(id_,!frame || frame->IsMain(),frame?frame->GetURL().ToString():std::string(),url.ToString(),0);
      OpenTab(url,disposition==CEF_WOD_NEW_BACKGROUND_TAB);return true;
    }
    return false;
  }
  CefRefPtr<CefDialogHandler> GetDialogHandler() override { return this; }
  CefRefPtr<CefJSDialogHandler> GetJSDialogHandler() override { return this; }
  // A page's own questions: alert, confirm, prompt and "leave this page?"
  // (`beforeunload`). The embedder that asked for them (`js_dialogs`) shows
  // each as a `js_dialog` and answers it with `js_dialog_reply`; for any
  // other embedder a question is declined at once and the page may always be
  // left, so a page never waits on a question nobody sees. A tab closing
  // lets its page go unasked.
  bool OnJSDialog(CefRefPtr<CefBrowser>,const CefString& origin_url,JSDialogType type,const CefString& message,const CefString& default_text,CefRefPtr<CefJSDialogCallback> callback,bool& suppress_message) override {
    suppress_message=false;
    if(!AsksDialogs()) {suppress_message=true;return false;}
    auto d=CefDictionaryValue::Create();
    d->SetString("kind",type==JSDIALOGTYPE_ALERT?"alert":type==JSDIALOGTYPE_CONFIRM?"confirm":"prompt");
    d->SetString("origin",tilo::Origin(origin_url.ToString()));
    d->SetString("message",tilo::BoundedText(message.ToString(),kMaxDialogText));
    d->SetString("default_text",type==JSDIALOGTYPE_PROMPT?tilo::BoundedText(default_text.ToString(),kMaxDialogText):std::string());
    AskDialog(callback,false,d);return true;
  }
  bool OnBeforeUnloadDialog(CefRefPtr<CefBrowser> browser,const CefString&,bool is_reload,CefRefPtr<CefJSDialogCallback> callback) override {
    if(!AsksDialogs()) {callback->Continue(true,CefString());return true;}
    // Chromium shows no text of the page's here: what it asks is fixed.
    auto d=CefDictionaryValue::Create();d->SetString("kind","beforeunload");d->SetBool("reload",is_reload);
    d->SetString("origin",tilo::Origin(browser->GetMainFrame()->GetURL().ToString()));d->SetString("message","");d->SetString("default_text","");
    AskDialog(callback,true,d);return true;
  }
  // Chromium let the question go (the page navigated or closed): it no
  // longer waits on an answer.
  void OnResetDialogState(CefRefPtr<CefBrowser>) override { CloseDialog(); }
  bool AsksDialogs() const { return js_dialogs_ && !transfer_owner_ && !native_closing_ && !closing; }
  void AskDialog(CefRefPtr<CefJSDialogCallback> callback,bool unload,CefRefPtr<CefDictionaryValue> d) {
    // Chromium asks one question of a page at a time; one still open is
    // declined before the next.
    if(dialog_callback_) {auto previous=dialog_callback_;const bool leave=dialog_unload_;CloseDialog();previous->Continue(leave,CefString());}
    dialog_id_=dialog_id_%2147483647+1;dialog_callback_=callback;dialog_unload_=unload;
    d->SetInt("id",dialog_id_);Event("js_dialog",d);
  }
  void CloseDialog() {
    if(!dialog_callback_) return;
    dialog_callback_=nullptr;auto d=CefDictionaryValue::Create();d->SetInt("id",dialog_id_);Event("js_dialog_closed",d);
  }
  // Cancel, and stay when the page asked to be left: the navigation that
  // asked is superseded by the one declining it.
  void DeclineDialog() {
    if(!dialog_callback_) return;
    auto callback=dialog_callback_;CloseDialog();callback->Continue(false,CefString());
  }
  void AnswerDialog(int id,bool accept,const CefString& text) {
    if(!dialog_callback_ || id!=dialog_id_) return;
    auto callback=dialog_callback_;const bool unload=dialog_unload_;CloseDialog();
    callback->Continue(accept,text);
    // Staying on the page cancels the navigation that asked: it never starts,
    // so nothing else would settle it.
    if(unload && !accept && navigation_dispatched_ && !navigation_started_) RefuseNavigation("The page asked to stay");
  }
  bool OnFileDialog(CefRefPtr<CefBrowser>,FileDialogMode mode,const CefString& title,const CefString&,const std::vector<CefString>&,const std::vector<CefString>& extensions,const std::vector<CefString>&,CefRefPtr<CefFileDialogCallback> callback) override {
#if defined(OS_MAC)
    Event("file_dialog");
    // An agent's page action cannot drive a native panel, so while it is
    // driving the chooser is held for fill-page-files instead. A person's
    // own click keeps the platform panel.
    if(page_automation_ && page_automation_->HoldFileDialog(callback)) return true;
    return ShowHostFileDialog(mode,title,extensions,callback);
#else
    return false;
#endif
  }
  void PublishZoom() {
    if(!browser_) return;
    const double level=browser_->GetHost()->GetZoomLevel();
    auto d=CefDictionaryValue::Create();d->SetDouble("level",level);Event("zoom",d);
    // Extensions hear of a change: from a tab's first page on.
    if(published_zoom_ && *published_zoom_!=level && extension_tabs)extension_tabs->Zoomed(id_,std::pow(1.2,*published_zoom_),std::pow(1.2,level));
    published_zoom_=level;
  }
  CefRefPtr<CefContextMenuHandler> GetContextMenuHandler() override { return this; }
  void OnBeforeContextMenu(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,CefRefPtr<CefContextMenuParams> params,CefRefPtr<CefMenuModel> model) override {
    if(!params->GetLinkUrl().empty()) { model->AddItem(26501,"Open link in new tab");model->AddItem(26502,"Copy link address"); }
    // Extensions' items, after the page's own as in Chrome.
    menu_offers_.clear();
    if(!extension_tabs) return;
    menu_context_=tilo::MenuContext{};
    menu_context_.page=params->GetPageUrl().ToString();menu_context_.frame=params->GetFrameUrl().ToString();
    menu_context_.link=params->GetLinkUrl().ToString();menu_context_.src=params->GetSourceUrl().ToString();
    menu_context_.selection=params->GetSelectionText().ToString();menu_context_.editable=params->IsEditable();
    menu_context_.main_frame=!frame || frame->IsMain();
    switch(params->GetMediaType()) {
      case CM_MEDIATYPE_IMAGE: menu_context_.media="image";break;
      case CM_MEDIATYPE_VIDEO: menu_context_.media="video";break;
      case CM_MEDIATYPE_AUDIO: menu_context_.media="audio";break;
      default: break;
    }
    const auto offers=extension_tabs->MenuOffers(menu_context_);
    if(!offers.empty() && model->GetCount()) model->AddSeparator();
    int group=0;
    AddMenuOffers(model,offers,group);
  }
  // Extensions' items into `model`, each chosen by its command from
  // kFirstMenuOffer: checkbox and radio items with their state, a run of
  // radio items as one group, and an item with items under it as a submenu.
  void AddMenuOffers(CefRefPtr<CefMenuModel> model,const std::vector<tilo::MenuOffer>& offers,int& group) {
    bool radio=false;
    for(const auto& offer:offers) {
      if(offer.type=="separator") { model->AddSeparator();radio=false;continue; }
      if(menu_offers_.size()>=kMaxMenuOffers) return;
      const int command=kFirstMenuOffer+static_cast<int>(menu_offers_.size());
      // The page menu reads `&` as marking a key; `&&` is one.
      std::string label;
      for(char c:offer.label) { label+=c;if(c=='&') label+='&'; }
      tilo::MenuOffer chosen=offer;chosen.children.clear();menu_offers_.push_back(std::move(chosen));
      if(!offer.children.empty()) AddMenuOffers(model->AddSubMenu(command,label),offer.children,group);
      else if(offer.type=="checkbox") model->AddCheckItem(command,label);
      else if(offer.type=="radio") { if(!radio) ++group;model->AddRadioItem(command,label,group); }
      else model->AddItem(command,label);
      radio=offer.type=="radio";
      if(offer.type=="checkbox" || offer.type=="radio") model->SetChecked(command,offer.checked);
      if(!offer.enabled) model->SetEnabled(command,false);
    }
  }
  // The menu's items as the Browser shows them: at most 24 at each level,
  // four levels deep and 200 in all, the page's own disabled items left out.
  CefRefPtr<CefListValue> MenuItems(CefRefPtr<CefMenuModel> model,int depth,size_t& left) {
    auto items=CefListValue::Create();
    for(size_t i=0;i<model->GetCount() && items->GetSize()<24 && left>0;++i) {
      const auto type=model->GetTypeAt(i);
      if(type==MENUITEMTYPE_SEPARATOR) {
        const size_t last=items->GetSize();
        if(last && !items->GetDictionary(last-1)->HasKey("separator")) { auto item=CefDictionaryValue::Create();item->SetBool("separator",true);items->SetDictionary(last,item);--left; }
        continue;
      }
      const int command=model->GetCommandIdAt(i);
      const bool extension=command>=kFirstMenuOffer && command<kFirstMenuOffer+static_cast<int>(kMaxMenuOffers);
      if(command<=0 || (!model->IsEnabledAt(i) && !extension)) continue;
      if(model->GetLabelAt(i).ToString().size()>4096) continue;
      auto item=CefDictionaryValue::Create();item->SetInt("id",command);item->SetString("label",model->GetLabelAt(i));
      if(!model->IsEnabledAt(i)) item->SetBool("enabled",false);
      --left;
      if(type==MENUITEMTYPE_SUBMENU) {
        auto children=depth<4 && model->GetSubMenuAt(i) ? MenuItems(model->GetSubMenuAt(i),depth+1,left) : CefListValue::Create();
        if(!children->GetSize()) { ++left;continue; }
        item->SetList("items",children);
      } else {
        if(type==MENUITEMTYPE_CHECK || type==MENUITEMTYPE_RADIO) item->SetBool("checked",model->IsCheckedAt(i));
        if(model->IsEnabledAt(i)) context_commands_.insert(command);
      }
      items->SetDictionary(items->GetSize(),item);
    }
    const size_t last=items->GetSize();
    if(last && items->GetDictionary(last-1)->HasKey("separator")) { items->Remove(last-1);++left; }
    return items;
  }
  bool RunContextMenu(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,CefRefPtr<CefContextMenuParams> params,CefRefPtr<CefMenuModel> model,CefRefPtr<CefRunContextMenuCallback> callback) override {
    if(context_callback_) context_callback_->Cancel();
    if(params->GetLinkUrl().ToString().size()>8192) { callback->Cancel();return true; }
    context_callback_=callback;context_id_=context_id_%2147483647+1;context_commands_.clear();
    auto d=CefDictionaryValue::Create();d->SetInt("id",context_id_);d->SetInt("x",params->GetXCoord());d->SetInt("y",params->GetYCoord());d->SetString("link",params->GetLinkUrl());
    size_t left=200;d->SetList("items",MenuItems(model,1,left));Event("context_menu",d);return true;
  }
  bool OnContextMenuCommand(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>,CefRefPtr<CefContextMenuParams> params,int command,EventFlags) override {
    if(command==26501) { OpenTab(params->GetLinkUrl(),false);return true; }
    if(command>=kFirstMenuOffer && command<kFirstMenuOffer+static_cast<int>(menu_offers_.size())) {
      if(extension_tabs) extension_tabs->MenuChosen(id_,menu_offers_[static_cast<size_t>(command-kFirstMenuOffer)],menu_context_);
      return true;
    }
    return false;
  }
  // Answer a page menu still open: Chromium closing the browser with it
  // pending crashes the host, a tab closed alone as much as the host's
  // last.
  void CancelMenu() { if(context_callback_){auto callback=context_callback_;context_callback_=nullptr;callback->Cancel();} }
  void OnContextMenuDismissed(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame>) override { context_callback_=nullptr;context_commands_.clear();auto d=CefDictionaryValue::Create();d->SetInt("id",context_id_);Event("context_closed",d); }
  CefRefPtr<CefFindHandler> GetFindHandler() override { return this; }
  void OnFindResult(CefRefPtr<CefBrowser>,int identifier,int count,const CefRect& selection,int active,bool final_update) override {
    if(final_update && count>0 && active==0 && select_initial_) {
      select_initial_=false;
      CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self){if(self->browser_ && self->finding_) self->browser_->GetHost()->Find(self->find_text_,true,false,true);},CefRefPtr<Client>(this)));
      return;
    }
    auto d=CefDictionaryValue::Create();d->SetInt("id",find_id_);d->SetInt("count",count);d->SetInt("active",active);d->SetBool("final",final_update);Event("find_result",d);
    // Where the active match is in the view, for an embedder that scrolls
    // the page itself (a mail sized to its whole document).
    if(selection.width>0 && selection.height>0) { auto r=CefDictionaryValue::Create();r->SetInt("id",find_id_);r->SetDouble("x",selection.x);r->SetDouble("y",selection.y);r->SetDouble("width",selection.width);r->SetDouble("height",selection.height);Event("find_selection",r); }
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
  CefRefPtr<CefDragHandler> GetDragHandler() override { return this; }
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
  // Where the text being composed is, for the input method's candidates.
  // Chromium reports each character's box in view DIPs; the protocol's
  // pixels are physical. Only the composition's first line counts: a box
  // around a wrapped composition would push the candidates off it.
  void OnImeCompositionRangeChanged(CefRefPtr<CefBrowser>,const CefRange& range,const RectList& bounds) override {
    auto d=CefDictionaryValue::Create();
    if(range.from!=CefRange::InvalidRange().from && !bounds.empty() && bounds.front().height>0) {
      const CefRect& first=bounds.front();
      int left=first.x,top=first.y,right=first.x+first.width,bottom=first.y+first.height;
      for(const auto& box:bounds) {
        if(box.height<=0 || box.y>=bottom || box.y+box.height<=top) continue;
        left=std::min(left,box.x);right=std::max(right,box.x+box.width);
      }
      const double scale=geometry_.scale();
      d->SetDouble("x",left*scale);d->SetDouble("y",top*scale);d->SetDouble("width",(right-left)*scale);d->SetDouble("height",(bottom-top)*scale);
    }
    PublishComposition(d);
  }
  // A composition's box, or none (no `x`) once it has ended.
  void PublishComposition(CefRefPtr<CefDictionaryValue> d) {
    auto value=CefValue::Create();value->SetDictionary(d->Copy(false));
    const auto report=CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString();
    if(report==composition_report_)return;
    composition_report_=report;Event("ime_composition_bounds",d);
  }
  // The caret moved, or a selection changed: the focused field may have
  // moved with it (a text area growing as it is typed in). Typing changes
  // the selection with every key; one probe answers a burst of them.
  void OnTextSelectionChanged(CefRefPtr<CefBrowser>,const CefString&,const CefRange&) override { ProbeTextFieldSoon(); }
  // Ask the focused frame's renderer for its focused field (renderer.cc).
  void ProbeTextField() {
    if(!browser_)return;auto frame=browser_->GetFocusedFrame();
    if(frame && frame->IsValid()) frame->SendProcessMessage(PID_RENDERER,CefProcessMessage::Create("tilo.text_field_probe"));
  }
  // Once the page has settled from a scroll, a resize or typing, while a
  // field has the focus: one probe however many events arrive in between.
  void ProbeTextFieldSoon() {
    if(!text_field_editable_ || text_field_probe_scheduled_)return;
    text_field_probe_scheduled_=true;
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self){ self->text_field_probe_scheduled_=false;self->ProbeTextField(); },CefRefPtr<Client>(this)),100);
  }
  // A renderer's report of its focused field: on a focus change, or in
  // answer to a probe. A probe's answer counts only from the frame that has
  // the focus now; a stale one from a frame left behind is dropped.
  // Renderer input is untrusted: every value is checked and bounded.
  void ReceiveTextField(CefRefPtr<CefFrame> frame,CefRefPtr<CefListValue> args) {
    if(!browser_ || !frame || !frame->IsValid())return;
    const size_t size=args->GetSize();
    if((size!=2 && size!=6) || args->GetType(0)!=VTYPE_BOOL || args->GetType(1)!=VTYPE_INT)return;
    for(size_t i=2;i<size;++i)if(args->GetType(i)!=VTYPE_INT || std::abs(args->GetInt(i))>65536)return;
    if(args->GetBool(0)) {auto focused=browser_->GetFocusedFrame();if(!focused || focused->GetIdentifier()!=frame->GetIdentifier())return;}
    static const char* const purposes[]={"","text","text","password","email","url","phone","number","search"};
    const int kind=args->GetInt(1);
    if(kind<0 || kind>=static_cast<int>(std::size(purposes)))return;
    auto d=CefDictionaryValue::Create();d->SetBool("editable",kind!=0);
    if(kind) {
      d->SetString("purpose",purposes[kind]);d->SetBool("multiline",kind==2);
      // The renderer gives the box in the widget's device pixels, which are
      // the protocol's physical pixels.
      if(size==6 && args->GetInt(5)>0 && args->GetInt(4)>=0) {
        d->SetInt("x",args->GetInt(2));d->SetInt("y",args->GetInt(3));d->SetInt("width",args->GetInt(4));d->SetInt("height",args->GetInt(5));
      }
    }
    PublishTextField(d);
  }
  void PublishTextField(CefRefPtr<CefDictionaryValue> d) {
    auto value=CefValue::Create();value->SetDictionary(d->Copy(false));
    const auto report=CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString();
    if(report==text_field_report_)return;
    text_field_report_=report;text_field_editable_=d->GetBool("editable");Event("text_field",d);
  }
  // A drag the engine opened for a gesture that had already let go ends here.
  static void EndOpenedDrag(CefRefPtr<CefBrowser> browser) {
    if(!browser) return;
    auto host=browser->GetHost();
    host->DragSourceEndedAt(0,0,DRAG_OPERATION_NONE);
    host->DragSourceSystemDragEnded();
  }
  // Windowless: the page's drag begins here and ends when the gesture does.
  // A windowless view has no file to offer an editor, so the page's text
  // moves, which is what the gesture asked for.
  bool StartDragging(CefRefPtr<CefBrowser>,CefRefPtr<CefDragData> data,CefRenderHandler::DragOperationsMask allowed_ops,int x,int y) override {
    if(!data) return false;
    // A gesture this automation drives carries its own drag: its press, its
    // moves and its release are the pointer steps already sent, and the page
    // has already heard the drop. The engine opens its drag a turn after those
    // steps have gone, so there is nothing left to carry: it is ended here at
    // once, or the page is left holding a drag nobody lets go of and reads
    // every input after it as part of that drag.
    if(page_automation_ && page_automation_->DrivesDrag()) {
      CefMouseEvent at;at.x=x;at.y=y;at.modifiers=EVENTFLAG_LEFT_MOUSE_BUTTON;
      CefRefPtr<CefBrowser> browser=browser_;
      CefPostDelayedTask(TID_UI,base::BindOnce(&EndOpenedDrag,browser),0);
      (void)at;
      return true;
    }
    if(data->IsFile() && !data->IsFragment()) data->ResetFileContents();
    dragging_=data;dragging_entered_=false;
    // The drag begins where the hand is, so the page hears it enter there
    // and every later move is an over.
    CefMouseEvent at;at.x=x;at.y=y;at.modifiers=EVENTFLAG_LEFT_MOUSE_BUTTON;
    browser_->GetHost()->DragTargetDragEnter(dragging_,at,kPageDragOps);
    browser_->GetHost()->DragTargetDragOver(at,kPageDragOps);
    dragging_entered_=true;
    return true;
  }
  void GetViewRect(CefRefPtr<CefBrowser>,CefRect& rect) override { rect=CefRect(0,0,geometry_.logical_width(),geometry_.logical_height()); }
  bool GetScreenInfo(CefRefPtr<CefBrowser>,CefScreenInfo& info) override {
    info.device_scale_factor=geometry_.scale();info.rect=info.available_rect=CefRect(0,0,geometry_.logical_width(),geometry_.logical_height());return true;
  }
  void OnAfterCreated(CefRefPtr<CefBrowser> browser) override {
    browser_=browser;
    if(id_==1 && tilo::StartupDiagnostics::Enabled()){startup_diagnostics_=new tilo::StartupDiagnostics(browser);startup_diagnostics_->Attach();}
    request_filter->Begin(browser->GetIdentifier(),"about:blank");
    site_clear_=new tilo::SiteClear(browser,[this](int request,const std::string& origin,uint64_t generation,bool success,const std::string& message){SiteResult(request,origin,generation,"clear",success,message);});site_clear_->Attach();
    document_scripts_=new tilo::DocumentScripts(browser,[this] { CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<Client> self){ self->FlushNavigation(); },CefRefPtr<Client>(this))); },[this] { if(native_closing_)return;StringEvent("error","message","Document-start script registration failed");CloseHost(); });
    document_scripts_->Attach();
    // Register before releasing navigation. A late private context must not
    // infer idle composition from the absence of an event it could not observe.
    // A registration cut short by its tab closing (DevTools detaches with
    // it) leaves nothing unobserved.
    composition_scripts_=new tilo::DocumentScripts(browser,[this] { ScheduleNavigation(); },
        [this] { if(native_closing_)return;StringEvent("error","message","Private composition observer registration failed");CloseHost(); },
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
        [this](double x,double y){PageClick(x,y);},[this](CefRefPtr<CefDictionaryValue> key){PageKey(key);},
        [this](CefRefPtr<CefDictionaryValue> event){PagePointer(event);},[this](double x,double y,double dy){PageWheel(x,y,dy);},
        [this](){PageActionBegan();},[this](){PageActionEnded();});
    page_automation_->Attach();
    // A page's geolocation call reaches the person as its other permission
    // requests do: the engine has no location of Chromium's own to grant.
    geolocation_=new tilo::Geolocation(browser,[this](const std::string& origin,int request) {
      capabilities_->AskEngine(origin,"location",[this,origin,request](bool allow) {
        if(!geolocation_) return;
        if(!allow) {geolocation_->Answer(origin,request,std::nullopt);return;}
        // A test or development session names one position; otherwise the
        // machine answers with the person's own, or with nothing.
        if(auto fixed=tilo::FixedPosition()) {geolocation_->Answer(origin,request,fixed);return;}
        tilo::SystemPosition([this,origin,request](std::optional<tilo::Position> position) {
          if(geolocation_) geolocation_->Answer(origin,request,position);
        });
      });
    });
    geolocation_->Attach();
    agent_log_=new tilo::AgentLog(browser,[this](const std::string& name,CefRefPtr<CefDictionaryValue> fields){Event(name,fields);});
    agent_log_->Attach();
    inspector_=new tilo::TabInspector(browser);
    // An extension's own pages (its popup, its settings) answer the tab
    // APIs; the script does nothing in any other page.
    extension_scripts_=new tilo::DocumentScripts(browser,[] {},[this] { if(native_closing_)return;StringEvent("error","message","Extension page script registration failed"); });
    extension_scripts_->Attach();
    extension_scripts_->Configure(tilo::kExtensionTabs);
    if(extension_tabs)extension_tabs->Created(id_);
    paint_snapshot_=new tilo::PaintSnapshot(browser,[this](const std::string& name,CefRefPtr<CefDictionaryValue> fields){Event(name,fields);});
    paint_snapshot_->Attach();
    document_size_=new tilo::DocumentSize(browser,[this](const std::string& name,CefRefPtr<CefDictionaryValue> fields){Event(name,fields);});
    document_size_->Attach();
    auto d=CefDictionaryValue::Create();d->SetBool("native_edit_history",true);d->SetBool("guarded_edit_history",true);d->SetBool("native_caret_text_input",true);d->SetBool("ime_composition",true);d->SetBool("document_size",true);d->SetBool("popups",true);d->SetBool("js_dialogs",true);d->SetBool("selection_text",true);d->SetBool("page_clipboard",page_clipboard && !sealed_documents);d->SetBool("sealed_documents",sealed_documents);d->SetInt("frame_rate",kOffscreenFrameRate);d->SetString("cef_version",CEF_VERSION);d->SetString("chromium_version",std::to_string(CHROME_VERSION_MAJOR)+"."+std::to_string(CHROME_VERSION_MINOR)+"."+std::to_string(CHROME_VERSION_BUILD)+"."+std::to_string(CHROME_VERSION_PATCH));Event(id_==1?"ready":"tab_ready",d);
    if(extension_tabs)extension_tabs->Introduce(id_);
    TraceGeometry("ready",id_);
    browser_->GetHost()->WasHidden(!visible_);
    if(closing) browser_->GetHost()->CloseBrowser(true);
  }
  // The browser is closing, also of its own accord (a popup's
  // window.close()): DevTools detaches as it goes, which cuts short any
  // script registration still under way, and that is no failure of the host.
  bool DoClose(CefRefPtr<CefBrowser>) override { native_closing_=true;return false; }
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
    if(geolocation_)geolocation_->Detach();
    geolocation_=nullptr;
    if(page_automation_)page_automation_->Detach();
    page_automation_=nullptr;
    if(agent_log_)agent_log_->Detach();
    agent_log_=nullptr;
    if(extension_tabs && !side_panel_.empty())extension_tabs->SidePanelClosed(id_);
    if(extension_tabs)extension_tabs->Removed(id_);
    if(inspector_)inspector_->Detach();
    inspector_=nullptr;
    if(extension_scripts_)extension_scripts_->Detach();
    extension_scripts_=nullptr;
    if(last_focused_tab==id_)last_focused_tab=0;
    if(paint_snapshot_)paint_snapshot_->Detach();
    paint_snapshot_=nullptr;
    if(document_size_)document_size_->Detach();
    document_size_=nullptr;
    if(startup_diagnostics_)startup_diagnostics_->Detach();
    startup_diagnostics_=nullptr;
    if(document_scripts_)document_scripts_->Detach();
    document_scripts_=nullptr;
    if(composition_scripts_)composition_scripts_->Detach();
    composition_scripts_=nullptr;
    if(service_session_import_)service_session_import_->Detach();service_session_import_=nullptr;
    browser_=nullptr;
    download_owners.Destroy(id_);
    // Its popups go with it: nothing is left for them to answer.
    std::vector<CefRefPtr<Client>> popups;
    for(const auto& [id,client]:clients)if(client->opener_==id_)popups.push_back(client);
    for(const auto& popup:popups)popup->CloseNative();
    Event("closed");clients.erase(id_);PublishDownloadRuntime();
    if(closing && clients.empty())CefQuitMessageLoop();
  }
  bool OnBeforeBrowse(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,CefRefPtr<CefRequest> request,bool user_gesture,bool redirect) override {
    // A sign-in panel going to its extension's own address ends the sign-in
    // there: the page goes no further, and the panel closes.
    if(extension_panel_ && frame->IsMain() && extension_tabs && extension_tabs->SignInNavigating(id_,request->GetURL().ToString())) return true;
    if(sealed_documents && !SealedDocumentUrl(request->GetURL().ToString())) {
      // A link the person followed opens with the embedder; a refresh, a
      // redirect or a frame goes nowhere.
      if(frame->IsMain() && user_gesture && !redirect) OpenTab(request->GetURL(),false);
      return true;
    }
    if(frame->IsMain()) { if(navigation_dispatched_ && (navigation_target_.empty() || navigation_started_ || request->GetURL()==navigation_target_)) { navigation_started_=true; navigation_urls_.insert(request->GetURL().ToString()); } else if(!redirect) { navigation_busy_=true; } TraceGeometry("before-browse",id_);if(page_automation_)page_automation_->NavigationStarted();if(agent_log_ && !redirect)agent_log_->MainNavigation(request->GetURL().ToString(),user_gesture);InvalidateCredentials();request_filter->Begin(browser->GetIdentifier(),request->GetURL());capabilities_->Close(); }return false;
  }
  bool OnBeforePopup(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,int,const CefString& url,const CefString&,cef_window_open_disposition_t disposition,bool user_gesture,const CefPopupFeatures& features,CefWindowInfo& window,CefRefPtr<CefClient>& client,CefBrowserSettings& settings,CefRefPtr<CefDictionaryValue>&,bool*) override {
    if(sealed_documents && !user_gesture) return true;
    // A popup window the page opens (window.open with a size: a sign-in, a
    // payment) stays its popup, so it can answer the page that opened it.
    // The app shows it beside the page, where it asked for them.
    if(disposition==CEF_WOD_NEW_POPUP && popups_ && !sealed_documents && !closing && clients.size()<64 && url.length()<=8192 && next_popup<INT32_MAX) {
      const int id=next_popup++;
      auto popup=CefRefPtr<Client>(new Client(id));popup->opener_=id_;clients.emplace(id,popup);
      window.SetAsWindowless(0);client=popup;
      settings.windowless_frame_rate=kOffscreenFrameRate;settings.background_color=CefColorSetARGB(255,255,255,255);
      auto d=CefDictionaryValue::Create();d->SetInt("popup",id);d->SetString("url",url);
      d->SetInt("width",features.widthSet?std::clamp(features.width,0,10000):0);d->SetInt("height",features.heightSet?std::clamp(features.height,0,10000):0);
      Event("popup",d);
      if(extension_tabs)extension_tabs->Opening(id_,!frame || frame->IsMain(),frame?frame->GetURL().ToString():std::string(),url.ToString(),id);
      return false;
    }
    if(extension_tabs && url.length()<=8192)extension_tabs->Opening(id_,!frame || frame->IsMain(),frame?frame->GetURL().ToString():std::string(),url.ToString(),0);
    OpenTab(url,disposition==CEF_WOD_NEW_BACKGROUND_TAB);return true;
  }
  void OnAddressChange(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,const CefString& url) override { if(frame->IsMain()) { if(navigation_dispatched_ && (navigation_target_.empty() || navigation_started_ || url==navigation_target_)) { navigation_started_=true; navigation_urls_.insert(url.ToString()); } main_navigation_finished_=false;++favicon_generation_;std::optional<std::pair<std::string,std::pair<uint64_t,bool>>> pending;if(pending_main_media_token_){const auto key=frame->GetIdentifier().ToString();auto found=media_frames_.find(key);if(found!=media_frames_.end() && found->second.first==*pending_main_media_token_)pending=std::make_pair(key,std::make_pair(found->second.first,false));}media_frames_.clear();if(pending)media_frames_.insert(*pending);pending_main_media_token_.reset();media_limit_reported_=false;last_media_playing_.reset();navigation_history_->Invalidate();StringEvent("uri","url",url);PublishMediaState(false);favicon_.clear();
      if(extension_tabs) {auto change=CefDictionaryValue::Create();change->SetString("url",url);extension_tabs->Updated(id_,change);extension_tabs->Opened(id_,url.ToString());}
      if(extension_tabs && !side_panel_.empty())extension_tabs->SidePanelShows(id_,panel_opener_,side_panel_,url.ToString()); } }
  void OnTitleChange(CefRefPtr<CefBrowser>,const CefString& title) override {
    StringEvent("title","title",title);title_=title.ToString();
    if(extension_tabs) {auto change=CefDictionaryValue::Create();change->SetString("title",title);extension_tabs->Updated(id_,change);}
  }
  // The address under the pointer, for a sealed document's embedder to
  // show before a link is followed.
  void OnStatusMessage(CefRefPtr<CefBrowser>,const CefString& value) override {
    if(!sealed_documents) return;
    auto text=value.ToString();if(text.length()>8192) text.clear();
    StringEvent("link_status","url",text);
  }
  // The pointer over the page takes the shape the page asks for: a hand on
  // a link, an I-beam on text. Sent by its CSS name, once per change.
  bool OnCursorChange(CefRefPtr<CefBrowser>,CefCursorHandle,cef_cursor_type_t type,const CefCursorInfo&) override {
    const char* name=CursorName(type);
    if(cursor_!=name) {cursor_=name;StringEvent("cursor","cursor",cursor_);}
    return true;
  }
  void OnFaviconURLChange(CefRefPtr<CefBrowser> browser,const std::vector<CefString>& urls) override {
    if(urls.empty() || urls.front().ToString().size()>8192) return;
    favicon_=urls.front().ToString();
    if(extension_tabs) {auto change=CefDictionaryValue::Create();change->SetString("favIconUrl",favicon_);extension_tabs->Updated(id_,change);}
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
    auto d=CefDictionaryValue::Create();d->SetBool("back",back && !tilo::OnlyBootstrapBehind(browser));d->SetBool("forward",forward);Event("history",d);
    if(extension_tabs) {auto change=CefDictionaryValue::Create();change->SetString("status",loading?"loading":"complete");extension_tabs->Updated(id_,change);}
    if(loading) { navigation_busy_=true; main_navigation_finished_=false; failed_main_navigation_.reset(); return; }
    // OnLoadEnd excludes same-document history; overall loading settles it.
    CompleteMainNavigation(browser->GetMainFrame()->GetURL().ToString(),0);
    // A sign-in out of sight ends once its page loaded.
    if(extension_panel_ && extension_tabs)extension_tabs->SignInLoaded(id_,browser->GetMainFrame()->GetURL().ToString());
    // Not at OnLoadEnd: the browser still counts as loading there, and an
    // action's reply waiting on the load would say it is still loading.
    if(page_automation_)page_automation_->NavigationFinished();
    if(!navigation_dispatched_ || navigation_started_ || navigation_aborting_) {
      const auto uri=browser->GetMainFrame()->GetURL().ToString();
      if(navigation_owner_) NavigationOutcome(*navigation_owner_,uri,navigation_error_);
      navigation_owner_.reset();navigation_dispatched_=false;navigation_started_=false;navigation_busy_=false;navigation_aborting_=false;bootstrap_settled_=true;navigation_urls_.clear();navigation_error_.reset();
      ScheduleNavigation();
    }
  }
  void OnLoadStart(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,TransitionType) override {
    if(!frame->IsMain()) return;
    tilo::ForgetBootstrap(browser);
    // A new document has no field focused yet.
    {auto field=CefDictionaryValue::Create();field->SetBool("editable",false);PublishTextField(field);}
    // A document in a new renderer has a widget of its own, which Chromium
    // does not tell it has the focus: without it the page takes keys but
    // ignores an input method's composition.
    if(page_focused_ && browser_) browser_->GetHost()->SetFocus(true);
    // A new document: extensions' worlds and ports in the old one are gone.
    if(extension_tabs)extension_tabs->Navigated(id_);
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
    if(service_session_import_)service_session_import_->Loaded(frame,status);
  }
  void OnLoadError(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,ErrorCode code,const CefString& text,const CefString& url) override {
    if(frame->IsMain()) { failed_main_navigation_=url.ToString(); InvalidateCredentials(); }
    if(!frame->IsMain()) return;
    if(navigation_owner_ && navigation_started_ && navigation_urls_.count(url.ToString())) navigation_error_=text.ToString();
    if(code!=ERR_ABORTED) {auto d=CefDictionaryValue::Create();d->SetString("url",url);d->SetString("message",text);Event("error",d);}
  }
  void OnPopupShow(CefRefPtr<CefBrowser>,bool show) override { popup_visible_=show;if(!show) { popup_.clear(); EmitFrame(); } }
  void OnPopupSize(CefRefPtr<CefBrowser>,const CefRect& rect) override { popup_rect_=rect; }
  void OnPaint(CefRefPtr<CefBrowser> browser,PaintElementType type,const RectList& dirty,const void* buffer,int width,int height) override {
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
    if(type==PET_VIEW) {
      stale_paint_recovery_pending_=false;
      // The paint's one copy: straight into the packet the output sends, which
      // the tab keeps to composite a popup over and to send again.
      auto packet=tilo::FramePacket(static_cast<uint32_t>(id_),width,height,static_cast<const char*>(buffer),size);if(packet.empty()) return;
      frame_=std::make_shared<const std::string>(std::move(packet));pixel_width_=width;pixel_height_=height;if(document_size_)document_size_->Painted();
      // What Chromium repainted, in the frame's pixels.
      std::vector<tilo::DamageRect> damage;
      for(const auto& rect:dirty) {
        const int64_t x0=std::max(0,rect.x),y0=std::max(0,rect.y),x1=int64_t(rect.x)+rect.width,y1=int64_t(rect.y)+rect.height;
        if(x1>x0 && y1>y0) damage.push_back({uint32_t(x0),uint32_t(y0),uint32_t(x1-x0),uint32_t(y1-y0)});
      }
      EmitFrame(&damage);return;
    }
    popup_.assign(static_cast<const char*>(buffer),size);popup_width_=width;popup_height_=height;
    EmitFrame();
  }
  // Publishes the tab's frame; `damage` says what changed since the frame
  // before it, null when that is not known.
  void EmitFrame(const std::vector<tilo::DamageRect>* damage=nullptr) {
    if(!visible_ || !frame_) return;
    TraceGeometry("frame-published",id_,pixel_width_,pixel_height_,geometry_.scale());
    if(whole_next_) {damage=nullptr;whole_next_=false;}
    if(!popup_visible_ || popup_.empty()) { output->SendFrame(static_cast<uint32_t>(id_),frame_,pixel_width_,pixel_height_,damage); return; }
    // The popup's pixels are in this frame only: the next one goes whole.
    whole_next_=true;
    std::string body=*frame_;const size_t offset=tilo::FramePixelOffset(static_cast<uint32_t>(id_));
    int x=static_cast<int>(std::round(popup_rect_.x*geometry_.scale())),y=static_cast<int>(std::round(popup_rect_.y*geometry_.scale()));
    for(int row=0;row<popup_height_;++row) {
      int dest_y=y+row;if(dest_y<0 || dest_y>=pixel_height_) continue;
      int start=std::max(0,-x),end=std::min(popup_width_,pixel_width_-x);
      if(end>start) std::memcpy(body.data()+offset+(size_t(dest_y)*pixel_width_+x+start)*4,popup_.data()+(size_t(row)*popup_width_+start)*4,size_t(end-start)*4);
    }
    output->SendFrame(static_cast<uint32_t>(id_),std::make_shared<const std::string>(std::move(body)),pixel_width_,pixel_height_);
  }
  bool OnProcessMessageReceived(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame> frame,CefProcessId source,CefRefPtr<CefProcessMessage> message) override {
    if(source!=PID_RENDERER) return false;
    if(message->GetName()=="tilo.text_field") {
      if(browser && browser_ && browser->IsSame(browser_)) ReceiveTextField(frame,message->GetArgumentList());
      return true;
    }
    if(message->GetName()=="tilo.selection_text") {
      auto args=message->GetArgumentList();
      if(args->GetSize()!=2 || args->GetType(0)!=VTYPE_INT || args->GetType(1)!=VTYPE_STRING) return true;
      auto d=CefDictionaryValue::Create();d->SetInt("request_id",args->GetInt(0));d->SetString("text",args->GetString(1));Event("selection_text",d);return true;
    }
    if(message->GetName()=="tilo.credential_result") {
      ReceiveCredentialResult(browser,frame,message->GetArgumentList());return true;
    }
    if(message->GetName()=="tilo.clipboard_write" || message->GetName()=="tilo.clipboard_read" || message->GetName()=="tilo.clipboard_primed") {
      if(page_clipboard && browser_ && browser && browser->IsSame(browser_) && frame && frame->IsValid()) PageClipboardMessage(frame,message);
      return true;
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
      } else if(action=="reset") {
        // Back to asking: what was blocked or allowed for the site is forgotten,
        // and so are the questions set aside, which after a few make Chromium
        // refuse the site unasked while site information still says it asks.
        auto context=browser_->GetHost()->GetRequestContext();for(const auto& permission:tilo::kSitePermissions)context->SetContentSetting(origin,origin,permission.type,CEF_CONTENT_SETTING_VALUE_DEFAULT);
        context->SetWebsiteSetting(origin,origin,CEF_CONTENT_SETTING_TYPE_PERMISSION_AUTOBLOCKER_DATA,nullptr);
        SiteResult(request,origin,document->generation,action,true,"The site asks again for what it needs.");
      } else if(action=="enable" || action=="disable") {
        auto origins=request_filter->Snapshot()->exceptions;if(action=="disable")origins.insert(origin);else origins.erase(origin);
        if(origins.size()>1024) {SiteResult(request,origin,document->generation,action,false,"Site exception limit reached");return false;}
        request_filter->Exceptions(origins);PublishFilters();SiteResult(request,origin,document->generation,action,true,"Protection preference changed");
      }
      return false;
    }
    auto state=request_filter->Snapshot();auto d=CefDictionaryValue::Create();d->SetInt("request_id",request);d->SetString("origin",actual);d->SetDouble("generation",document->generation);d->SetDouble("blocked",std::min<uint64_t>(document->blocked.load(),9007199254740991ULL));d->SetBool("global_enabled",state->enabled);d->SetBool("excepted",state->exceptions.count(actual));d->SetBool("effective",state->enabled && !state->exceptions.count(actual));d->SetBool("can_reset",true);
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
    else if(op=="back" && browser_->CanGoBack() && !tilo::OnlyBootstrapBehind(browser_))browser_->GoBack();
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
    if(op=="scoped_navigation" || op=="navigate" || op=="back" || op=="forward" || op=="reload" || op=="history_entry") {
      // Going elsewhere answers the page's open question: Chromium holds a
      // navigation back while the page waits on it.
      DeclineDialog();
      if(DeferNavigation(d))return;
    }
    auto host=browser_->GetHost();auto frame=browser_->GetMainFrame();
    if(op=="visibility") {
      const bool was_visible=visible_;
      visible_=d->GetBool("visible");
      // The window's word is the last one: an action that had the page shown
      // for its turn puts it back to what the window now says, not to what it
      // said when the action started.
      if(!visible_) shown_for_action_=false;
      credential_document_.SetVisible(visible_);
      if(!visible_) CancelCredentialRequests();
      // Its open page menu is answered before it hides: Chromium hiding a
      // page whose menu is answered afterwards takes the host down.
      if(!visible_) CancelMenu();
      host->WasHidden(!visible_);host->SetFocus(visible_);page_focused_=visible_;
      if(visible_) {
        host->Invalidate(PET_VIEW);
        // A background load may have completed without an eligible handshake.
        if(!was_visible && !credential_identity_) OfferCredentialContext(browser_->GetMainFrame());
      } else {frame_.reset();popup_.clear();whole_next_=true;}
    }
    // The app lost the frame its next rects would change: send it whole.
    else if(op=="resend_frame") {whole_next_=true;if(frame_)EmitFrame();else host->Invalidate(PET_VIEW);}
    else if(op=="js_dialogs") {
      js_dialogs_=d->GetBool("enabled");
      // An embedder that stops answering lets the open question go.
      if(!js_dialogs_ && dialog_callback_) AnswerDialog(dialog_id_,dialog_unload_,CefString());
    }
    else if(op=="js_dialog_reply") AnswerDialog(d->GetInt("id"),d->GetBool("accept"),d->HasKey("text")?d->GetString("text"):CefString());
    else if(op=="context_action") {
      // Choosing an item is the person's own act, as a click is: a copy
      // from the menu reaches the clipboard.
      activated_=std::chrono::steady_clock::now();
      if(context_callback_ && d->GetInt("id")==context_id_) {
        int command=d->GetInt("command");auto callback=context_callback_;context_callback_=nullptr;
        if(command && context_commands_.count(command)) callback->Continue(command,EVENTFLAG_NONE);else callback->Cancel();
      }
    }
    else if(op=="service_session_import") {
      CefRefPtr<Client> owner=this;
      auto reply=[owner](CefRefPtr<CefDictionaryValue> fields){
        if(!closing && output)owner->Event("service_session_result",fields);
      };
      if(private_mode || service_session_id.empty() || service_session_id!=d->GetString("session_id").ToString() || service_session_consumed) {
        auto fields=CefDictionaryValue::Create();fields->SetInt("request_id",d->GetInt("request_id"));
        fields->SetString("session_id",d->GetString("session_id"));fields->SetString("service",d->GetString("service"));
        fields->SetBool("imported",false);fields->SetInt("cookie_count",0);fields->SetInt("origin_storage_count",0);tilo::ForgetServiceCookies(d);reply(fields);return;
      }
      service_session_consumed=true;
      service_session_import_=new tilo::ServiceSessionImport(browser_,d,std::move(reply));
      service_session_import_->Start();
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
    else if(op=="document_size") document_size_->Enable(d->GetBool("enabled"));
    else if(op=="popups") popups_=d->GetBool("enabled");
    else if(op=="page_clipboard") shares_clipboard_=true;
    else if(op=="selection_text") {
      // The renderer reads the selection; the embedder puts it on the
      // system clipboard, which an offscreen engine cannot reach.
      auto frame=browser_->GetFocusedFrame();if(!frame) frame=browser_->GetMainFrame();
      auto message=CefProcessMessage::Create("tilo.selection");message->GetArgumentList()->SetInt(0,d->GetInt("request_id"));
      frame->SendProcessMessage(PID_RENDERER,message);
    }
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
    else if(op=="focus") {
      page_focused_=d->GetBool("focused");
      host->SetFocus(page_focused_);
      if(d->GetBool("focused") && !extension_panel_ && last_focused_tab!=id_) {last_focused_tab=id_;if(extension_tabs)extension_tabs->Focused(id_);}
    }
    // An extension's popup or settings page, shown over the page used last.
    else if(op=="extension_panel") {
      extension_panel_=true;panel_opener_=last_focused_tab;
      // Opened by its toolbar button: like a click, it may script the page.
      if(d->HasKey("action") && extension_tabs)extension_tabs->Grant(d->GetString("action"),panel_opener_);
      // An extension's side panel: it hears of the panel opening and
      // closing (extension_tabs.h, SidePanelShows).
      if(d->HasKey("side_panel"))side_panel_=d->GetString("side_panel").ToString();
    }
    // The page an extension asked the person to sign in on, shown in a
    // panel over the page used last or loaded out of sight: the host
    // watches where it goes. One for no sign-in going on closes.
    else if(op=="extension_sign_in_panel") {
      extension_panel_=true;panel_opener_=last_focused_tab;
      if(!extension_tabs || !extension_tabs->SignInPanel(d->GetString("id"),d->GetInt("flow"),id_))
        CefPostTask(TID_UI,base::BindOnce([](int tab){auto found=clients.find(tab);if(found!=clients.end())found->second->CloseNative();},id_));
    }
    // A click on an extension's toolbar button over this page.
    else if(op=="extension_click") {if(extension_tabs)extension_tabs->Clicked(d->GetString("id"),id_);}
    // An item of an extension's toolbar button menu, chosen over this page.
    else if(op=="extension_menu_action") {if(extension_tabs)extension_tabs->ActionMenuChosen(id_,d->GetString("id"),d->GetString("key"));}
    // What the person types after an extension's keyword in this page's address.
    else if(op=="extension_omnibox") {
      // The window the person types in is theirs now: an extension's
      // "current window" for what it does with the words.
      if(!extension_panel_ && last_focused_tab!=id_) {last_focused_tab=id_;if(extension_tabs)extension_tabs->Focused(id_);}
      if(extension_tabs)extension_tabs->Omnibox(d->GetString("id"),id_,d->GetString("input"),d->HasKey("text")?d->GetString("text").ToString():std::string(),d->HasKey("request")?d->GetInt("request"):0);
    }
    // The person opened an extension's side panel beside this page.
    else if(op=="extension_side_panel") {if(extension_tabs)extension_tabs->OpenSidePanel(d->GetString("id"),id_);}
    // One of extensions' keyboard shortcuts, pressed while the Browser's own
    // field had the keyboard: its command runs over this page, which sees
    // nothing of the key.
    else if(op=="extension_command_key") {
      if(auto shortcut=tilo::KeyShortcut(d->GetString("key"),d->GetBool("ctrl"),d->GetBool("alt"),d->GetBool("shift"),d->GetBool("meta")))
        if(extension_tabs)extension_tabs->Key(extension_panel_?panel_opener_:id_,*shortcut);
    }
    else if(op=="resize") {
      int w=d->GetInt("width"),h=d->GetInt("height"); double scale=Number(d,"scale_factor");
      if(!std::isfinite(scale)||scale<0.25||scale>8||!tilo::FrameSize(w,h)) { CloseHost(); return; }
      // IPC sizes and pointer coordinates are physical pixels; CEF uses DIP.
      const auto effect=geometry_.ResizePhysical(w,h,scale);
      TraceGeometry("resize-applied",id_,w,h,scale);
      stale_paint_recovery_pending_=false;
      if(effect==tilo::ResizeEffect::Repaint) { host->NotifyScreenInfoChanged();host->WasResized();host->Invalidate(PET_VIEW);return; }
      popup_.clear();frame_.reset();whole_next_=true;host->NotifyScreenInfoChanged();host->WasResized();host->Invalidate(PET_VIEW);ProbeTextFieldSoon();
    } else if(op=="mouse_move" || op=="mouse_button" || op=="scroll") {
      CefMouseEvent event;event.x=static_cast<int>(Number(d,"x")/geometry_.scale());event.y=static_cast<int>(Number(d,"y")/geometry_.scale());event.modifiers=mouse_modifiers_|key_modifiers_;
      if(op=="mouse_move") host->SendMouseMoveEvent(event,false);
      else if(op=="scroll") {
        // A trackpad moves in fractions of a pixel: carry what an integer
        // wheel event cannot, so slow scrolling adds up instead of vanishing.
        const double dx=Number(d,"dx")/geometry_.scale()+wheel_rest_x_,dy=Number(d,"dy")/geometry_.scale()+wheel_rest_y_;
        const int whole_x=static_cast<int>(dx),whole_y=static_cast<int>(dy);
        wheel_rest_x_=dx-whole_x;wheel_rest_y_=dy-whole_y;
        if(whole_x||whole_y) host->SendMouseWheelEvent(event,whole_x,whole_y);
        // A focused field scrolled moves under the input method.
        if(whole_x||whole_y) ProbeTextFieldSoon();
      }
      else { std::string button=d->GetString("button");auto type=button=="right"?MBT_RIGHT:button=="middle"?MBT_MIDDLE:MBT_LEFT;bool down=d->GetBool("down");
        int flag=type==MBT_RIGHT?EVENTFLAG_RIGHT_MOUSE_BUTTON:type==MBT_MIDDLE?EVENTFLAG_MIDDLE_MOUSE_BUTTON:EVENTFLAG_LEFT_MOUSE_BUTTON;
        if(down) mouse_modifiers_|=flag;else mouse_modifiers_&=~flag; event.modifiers=mouse_modifiers_|key_modifiers_;
        // A second and third press of the same button soon after, near the
        // first, is a double and a triple click: a word, then its paragraph.
        if(down) {
          const auto now=std::chrono::steady_clock::now();
          const bool again=click_count_>0 && type==click_button_ && now-click_time_<=std::chrono::milliseconds(500)
            && std::abs(event.x-click_x_)<=4 && std::abs(event.y-click_y_)<=4;
          click_count_=again?std::min(click_count_+1,3):1;
          click_button_=type;click_time_=now;click_x_=event.x;click_y_=event.y;activated_=now;
        }
        host->SendMouseClickEvent(event,type,!down,std::max(click_count_,1)); }
    } else if(op=="modifiers") key_modifiers_=(d->GetBool("ctrl")?EVENTFLAG_CONTROL_DOWN:0)|(d->GetBool("shift")?EVENTFLAG_SHIFT_DOWN:0)|(d->GetBool("alt")?EVENTFLAG_ALT_DOWN:0)|(d->GetBool("meta")?EVENTFLAG_COMMAND_DOWN:0);
    else if(op=="edit_history") {
      auto frame=browser_->GetFocusedFrame();
      if(frame) { if(d->GetString("direction")=="undo") frame->Undo();else frame->Redo(); }
    }
    else if(op=="key") {if(d->GetBool("down"))activated_=std::chrono::steady_clock::now();KeyCommand(d);}
    // The platform input method's composition, as a native field takes it:
    // the page sees compositionstart, compositionupdate and compositionend,
    // and the committed text as typed.
    else if(op=="ime_composition") {
      activated_=std::chrono::steady_clock::now();
      const CefString text=d->GetString("text");
      CefCompositionUnderline underline;
      // A transparent underline is drawn in the text's own colour.
      underline.range=CefRange(0,static_cast<uint32_t>(text.ToString16().size()));underline.color=0;underline.background_color=0;underline.thick=0;underline.style=CEF_CUS_SOLID;
      host->ImeSetComposition(text,{underline},CefRange::InvalidRange(),
        CefRange(static_cast<uint32_t>(d->GetInt("selection_start")),static_cast<uint32_t>(d->GetInt("selection_end"))));
    }
    else if(op=="ime_commit" || op=="ime_cancel" || op=="ime_finish") {
      if(op=="ime_commit") {activated_=std::chrono::steady_clock::now();host->ImeCommitText(d->GetString("text"),CefRange::InvalidRange(),0);}
      else if(op=="ime_cancel") host->ImeCancelComposition();
      else host->ImeFinishComposingText(false);
      // The composition is over: the next one is placed by its own box.
      PublishComposition(CefDictionaryValue::Create());
    }
    else if(op=="scripts" || op=="blocking_scripts") {auto& scripts=op=="scripts"?scripts_:blocking_scripts_;scripts.clear();auto list=d->GetList("scripts");if(list) for(size_t i=0;i<list->GetSize();++i) scripts.push_back(list->GetString(i));UpdateScriptPolicy();}
    else if(op=="permission_decision") capabilities_->PermissionDecision(d->GetInt("id"),d->GetBool("allow"),d->HasKey("dismiss") && d->GetType("dismiss")==VTYPE_BOOL && d->GetBool("dismiss"));
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
  // The page's clipboard where the engine has none (page_clipboard.h).
  // A copy reaches the Browser only within the person's activation of the
  // page, as Chrome lets a page write the clipboard; an extension's page
  // with `clipboardWrite` copies at any time.
  void PageClipboardMessage(CefRefPtr<CefFrame> frame,CefRefPtr<CefProcessMessage> message) {
    auto args=message->GetArgumentList();
    if(message->GetName()=="tilo.clipboard_primed") {
      if(args->GetSize()==1 && args->GetType(0)==VTYPE_INT && args->GetInt(0)==paste_prime_) ReleaseKeys();
      return;
    }
    if(message->GetName()=="tilo.clipboard_read") {
      if(args->GetSize()!=1 || args->GetType(0)!=VTYPE_INT) return;
      const int renderer=args->GetInt(0);
      // A read from a page the person can see, of a site the person allowed
      // to read the clipboard (or an extension's page that may): what the
      // page's script did cannot read it otherwise.
      const auto url=frame->GetURL().ToString();const std::string prefix="chrome-extension://";
      auto context=browser_->GetHost()->GetRequestContext();
      const bool allowed=(context && context->GetContentSetting(url,url,CEF_CONTENT_SETTING_TYPE_CLIPBOARD_READ_WRITE)==CEF_CONTENT_SETTING_VALUE_ALLOW) ||
        (url.rfind(prefix,0)==0 && extension_tabs && extension_tabs->MayReadClipboard(url.substr(prefix.size(),32)));
      const int request=visible_ && allowed && SharesClipboard()?clipboard_reads.Add({tilo::ClipboardReads::Purpose::Read,id_,frame->GetIdentifier().ToString(),renderer}):0;
      if(!request) {ClipboardText(frame,renderer,std::nullopt);return;}
      AskClipboard(request);
      return;
    }
    if(args->GetSize()!=3 || args->GetType(0)!=VTYPE_STRING || args->GetType(1)!=VTYPE_STRING || args->GetType(2)!=VTYPE_BOOL) return;
    const auto text=args->GetString(0).ToString();auto html=args->GetString(1).ToString();
    if(text.empty() || text.size()>tilo::kMaxClipboardText) return;
    if(html.size()>tilo::kMaxClipboardHtml) html.clear();
    const auto url=frame->GetURL().ToString();const std::string prefix="chrome-extension://";
    const bool extension=url.rfind(prefix,0)==0 && extension_tabs && extension_tabs->MayWriteClipboard(url.substr(prefix.size(),32));
    if((!activated() && !extension) || !SharesClipboard()) return;
    auto fields=CefDictionaryValue::Create();fields->SetString("text",text);
    if(!html.empty()) fields->SetString("html",html);
    fields->SetBool("secret",args->GetBool(2));
    ClipboardOwner()->Event("clipboard",fields);
  }
  // Who tells the Browser of a copy: the page itself, or for a page's
  // popup or an extension's panel the page it was opened from, which the
  // Browser shows.
  Client* ClipboardOwner() {
    const int owner=extension_panel_?panel_opener_:opener_;
    auto found=clients.find(owner);
    return owner && found!=clients.end() && found->second.get()!=this?found->second.get():this;
  }
  // The embedder takes this tab's copies and answers its reads
  // (`page_clipboard`); a tab of one that does not keeps the engine's own.
  bool SharesClipboard() {return page_clipboard && ClipboardOwner()->shares_clipboard_;}
  // Ask the Browser for the system clipboard's text, for `request`.
  void AskClipboard(int request) {
    auto fields=CefDictionaryValue::Create();fields->SetInt("request_id",request);
    ClipboardOwner()->Event("clipboard_read",fields);
    CefPostDelayedTask(TID_UI,base::BindOnce([](int request){ClipboardAnswer(request,std::nullopt);},request),tilo::kClipboardReadMs);
  }
  // The answer to a page's read, or nothing, which leaves it Chromium's.
  void ClipboardText(CefRefPtr<CefFrame> frame,int renderer,std::optional<std::string> text) {
    if(!frame || !frame->IsValid()) return;
    auto message=CefProcessMessage::Create("tilo.clipboard_text");auto args=message->GetArgumentList();
    args->SetInt(0,renderer);if(text)args->SetString(1,*text);else args->SetNull(1);
    frame->SendProcessMessage(PID_RENDERER,message);
  }
  void ClipboardRead(const tilo::ClipboardReads::Read& read,std::optional<std::string> text) {
    if(!browser_) return;
    if(read.purpose==tilo::ClipboardReads::Purpose::Read) {ClipboardText(browser_->GetFrameByIdentifier(read.frame),read.renderer,std::move(text));return;}
    if(read.renderer!=paste_prime_) return;
    // The system clipboard's text, kept in the focused frame for the paste
    // the key makes; without it the key pastes Chromium's own.
    auto frame=browser_->GetFocusedFrame();if(!frame) frame=browser_->GetMainFrame();
    if(!text || !frame) {ReleaseKeys();return;}
    auto message=CefProcessMessage::Create("tilo.clipboard_prime");
    message->GetArgumentList()->SetInt(0,paste_prime_);message->GetArgumentList()->SetString(1,*text);
    frame->SendProcessMessage(PID_RENDERER,message);
    CefPostDelayedTask(TID_UI,base::BindOnce([](int tab,int prime){auto found=clients.find(tab);if(found!=clients.end())found->second->PrimeLate(prime);},id_,paste_prime_),tilo::kClipboardPrimeMs);
  }
  void PrimeLate(int prime) {if(prime==paste_prime_)ReleaseKeys();}
  // A paste key waits for the system clipboard, and the keys after it with
  // it, so the page gets them in the order they were pressed.
  void KeyCommand(CefRefPtr<CefDictionaryValue> d) {
    if(paste_prime_) {if(held_keys_.size()<256)held_keys_.push_back(d->Copy(false));return;}
    // Ctrl+М on a Russian layout pastes as Ctrl+V does (key_mapping.h).
    const std::string key=d->GetString("key"),code=d->HasKey("code")?d->GetString("code").ToString():std::string();
    const char letter=key.size()==1?0:tilo::ShortcutLetter(key,code);
    if(SharesClipboard() && d->GetBool("down") && tilo::PasteKey(letter?std::string(1,letter):key,d->GetBool("ctrl"),d->GetBool("shift"),d->GetBool("alt"),d->GetBool("meta"))) {
      static int primes=0;primes=primes%2147483647+1;
      const int request=clipboard_reads.Add({tilo::ClipboardReads::Purpose::Paste,id_,{},primes});
      if(request) {paste_prime_=primes;held_keys_.push_back(d->Copy(false));AskClipboard(request);return;}
    }
    Key(d);
  }
  void ReleaseKeys() {
    paste_prime_=0;
    auto keys=std::move(held_keys_);held_keys_.clear();
    bool paste=true;
    for(auto& key:keys) {
      // The paste key itself goes on to the page; another after it waits
      // for the clipboard again.
      if(paste) {paste=false;Key(key);continue;}
      if(paste_prime_) held_keys_.push_back(key);else KeyCommand(key);
    }
  }
  void Key(CefRefPtr<CefDictionaryValue> d) {
    CefKeyEvent e;e.type=d->GetBool("down")?KEYEVENT_RAWKEYDOWN:KEYEVENT_KEYUP;
    e.modifiers=(d->GetBool("ctrl")?EVENTFLAG_CONTROL_DOWN:0)|(d->GetBool("shift")?EVENTFLAG_SHIFT_DOWN:0)|(d->GetBool("alt")?EVENTFLAG_ALT_DOWN:0)|(d->GetBool("meta")?EVENTFLAG_COMMAND_DOWN:0);
    key_modifiers_=e.modifiers;
    std::string key=d->GetString("key");
    const std::string code=d->HasKey("code")?d->GetString("code").ToString():std::string();
    // Shortcuts are named by Latin letters: on a layout without them the
    // letter is the one on the key pressed (Ctrl+С on a Russian layout).
    const char letter=tilo::ShortcutLetter(key,code);
    const bool latin=key.size()==1;
    const std::string shortcut_key=!latin && letter?std::string(1,letter):key;
    // An extension's keyboard shortcut runs its command, and the page gets
    // neither its press nor its release (extension_commands.h). A release
    // is known by the key's name: it may read otherwise ("y" once Shift is up).
    if(auto shortcut=tilo::KeyShortcut(shortcut_key,d->GetBool("ctrl"),d->GetBool("alt"),d->GetBool("shift"),d->GetBool("meta"))) {
      if(!d->GetBool("down")) { if(commanded_keys_.erase(shortcut->key)) return; }
      // In an extension's popup or side panel it runs over the page the
      // panel shows for, as in Chrome, whose shortcuts belong to the window.
      else if(extension_tabs && extension_tabs->Key(extension_panel_?panel_opener_:id_,*shortcut)) { commanded_keys_.insert(shortcut->key);return; }
    }
#if defined(OS_MAC)
    // Offscreen CEF has no Cocoa Edit menu to translate Command shortcuts.
    // Execute against the focused frame, preserving native clipboard semantics.
    if(d->GetBool("meta") && !d->GetBool("ctrl") && !d->GetBool("alt") && letter) {
      if(letter=='a'||letter=='c'||letter=='x'||letter=='v'||letter=='z') {
        if(d->GetBool("down")) { auto frame=browser_->GetFocusedFrame();if(frame) {
          if(letter=='a') frame->SelectAll();else if(letter=='c') frame->Copy();else if(letter=='x') frame->Cut();else if(letter=='v') frame->Paste();else if(d->GetBool("shift")) frame->Redo();else frame->Undo();
        }}
        return;
      }
    }
#endif
    const auto mapping=tilo::MapKey(key,code);
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
#if defined(OS_MAC)
    StampHostKeys(true);
#endif
    browser_->GetHost()->SendKeyEvent(e);
    if(d->GetBool("down") && (utf16.size()==1 || key=="Enter") && !d->GetBool("ctrl")&&!d->GetBool("meta")) { e.type=KEYEVENT_CHAR;browser_->GetHost()->SendKeyEvent(e); }
#if defined(OS_MAC)
    StampHostKeys(false);
#endif
  }
  // Page actions deliver input exactly as a person's pointer and keyboard do.
  // Coordinates arrive in CSS pixels; CEF's view space is zoomed DIPs.
  void PageClick(double x,double y) {
    if(!browser_)return;auto host=browser_->GetHost();const double zoom=std::pow(1.2,host->GetZoomLevel());
    CefMouseEvent event;event.x=static_cast<int>(std::lround(x*zoom));event.y=static_cast<int>(std::lround(y*zoom));event.modifiers=key_modifiers_;
    // The move and the click reach the page on separate turns: a point inside a
    // frame is routed to that frame only once the move that found it has been
    // handled, so sending both at once clicks the page and nothing in it.
    host->SendMouseMoveEvent(event,false);
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<CefBrowserHost> h,CefMouseEvent e) {
      e.modifiers|=EVENTFLAG_LEFT_MOUSE_BUTTON;h->SendMouseClickEvent(e,MBT_LEFT,false,1);
      e.modifiers&=~EVENTFLAG_LEFT_MOUSE_BUTTON;h->SendMouseClickEvent(e,MBT_LEFT,true,1);
    },host,event));
  }
  // One pointer step of a gesture: where a person's hand put the pointer.
  // A press that starts dragging a page element hands the drag to
  // StartDragging below, which the gesture's own moves then carry; the page
  // stays the target, so a drop lands where the hand let go.
  void PagePointer(CefRefPtr<CefDictionaryValue> step) {
    // A step reaches the page on a later turn than the one that read it, so
    // the step is kept as this host's own copy: CEF owns the dictionary only
    // for the callback that handed it over.
    step=step->Copy(true);
    if(!browser_)return;auto host=browser_->GetHost();const double zoom=std::pow(1.2,host->GetZoomLevel());
    // A pointer reaches the page only when the view is the one being used, just
    // as a key does: an app's window can be open without being focused, and its
    // page would then never see the pointer move.
    host->SetFocus(true);
    const auto kind=step->GetString("kind").ToString();
    const bool down=kind=="down",press=down||kind=="up";
    const bool right=step->GetString("button")=="right";
    const int button=right?EVENTFLAG_RIGHT_MOUSE_BUTTON:EVENTFLAG_LEFT_MOUSE_BUTTON;
    // The button a step presses stays held for the steps after it, so a move
    // travels as a hand's does: with the button still down.
    if(down) mouse_modifiers_|=button;
    // Modifiers describe this instant, so a release is reported without the
    // button that just let go.
    CefMouseEvent event;
    event.x=static_cast<int>(std::lround(Number(step,"x")*zoom));event.y=static_cast<int>(std::lround(Number(step,"y")*zoom));
    event.modifiers=mouse_modifiers_|key_modifiers_;
    if(dragging_) { DragPointer(kind,event);if(kind=="up")mouse_modifiers_&=~button;return; }
    host->SendMouseMoveEvent(event,false);
    // The page may have begun a drag on this very move: carry it to where the
    // hand went, so the element under the pointer hears the drag over it.
    if(dragging_) {host->DragTargetDragOver(event,kPageDragOps);return;}
    if(press) host->SendMouseClickEvent(event,right?MBT_RIGHT:MBT_LEFT,!down,1);
    if(kind=="up")mouse_modifiers_&=~button;
  }
  // A drag the page started is a target drag in every windowless view.
  void DragPointer(const std::string& kind,CefMouseEvent& event) {
    auto host=browser_->GetHost();
    if(kind=="up") {
      host->DragTargetDrop(event);
      host->DragSourceEndedAt(event.x,event.y,kPageDragOps);
      host->DragSourceSystemDragEnded();
      dragging_=nullptr;dragging_entered_=false;
      return;
    }
    if(kind=="down") return;
    if(!dragging_entered_) { host->DragTargetDragEnter(dragging_,event,kPageDragOps);dragging_entered_=true; }
    else host->DragTargetDragOver(event,kPageDragOps);
  }
  // One turn of the wheel a page action asks for, at a point of the page in
  // CSS pixels and `dy` CSS pixels down it: the same wheel a hand turns, so
  // the page's scroll listeners and intersection observers run for it and
  // content that loads on scroll arrives.
  void PageWheel(double x,double y,double dy) {
    if(!browser_)return;auto host=browser_->GetHost();const double zoom=std::pow(1.2,host->GetZoomLevel());
    CefMouseEvent event;event.x=static_cast<int>(std::lround(x*zoom));event.y=static_cast<int>(std::lround(y*zoom));
    event.modifiers=mouse_modifiers_|key_modifiers_;
    // A wheel notch is a whole number of pixels, and what is left over is
    // carried, as the scroll op does for a trackpad's fractions.
    const double moved=dy*zoom+wheel_rest_y_;
    const int whole=static_cast<int>(std::lround(moved));
    wheel_rest_y_=moved-whole;
    // The wheel reaches only the view being used, as a key does.
    host->SetFocus(true);
    // A wheel turn too small to round to a pixel is still a turn: the browser
    // is told the pixel it rounds to, so a page waiting on a scroll hears it.
    host->SendMouseWheelEvent(event,0,whole || !dy ? whole : (dy < 0 ? -1 : 1));
  }
  // An action runs over the page as if the caller were looking at it: a page
  // the window is not showing takes no wheel, no caret and no navigation, so
  // a tab that is in the background would answer every action with nothing.
  // The page is put back as the window had it once the action answers.
  void PageActionBegan() {
    if(!browser_ || visible_ || shown_for_action_) return;
    shown_for_action_=true;
    visible_=true;
    browser_->GetHost()->WasHidden(false);
  }
  void PageActionEnded() {
    if(!browser_ || !shown_for_action_) return;
    shown_for_action_=false;
    visible_=false;
    browser_->GetHost()->WasHidden(true);
  }
  void PageKey(CefRefPtr<CefDictionaryValue> key) {
    // A key of a fill's own typing arrives marked: it names its character, not
    // a key to press.
    if(key->HasKey("typed")) { TypedKey(key);return; }
    if(!browser_)return;const int held=key_modifiers_;
    // Keys reach only a focused view; the page's own focused element is kept.
    browser_->GetHost()->SetFocus(true);
    key->SetBool("down",true);Key(key);key->SetBool("down",false);Key(key);
    key_modifiers_=held;
  }
  // One key of a fill's own typing, sent as the browser sends a person's: the
  // press, the character it writes and the release, so the page hears keydown,
  // keypress and, before anything is written, its beforeinput.
  void TypedKey(CefRefPtr<CefDictionaryValue> key) {
    if(!browser_)return;const int held=key_modifiers_;
    // Keys reach only a focused view; the page's own focused element is kept.
    browser_->GetHost()->SetFocus(true);
    auto pressed=key->Copy(false);
    // A line break is the Enter key itself: its press is what the page hears,
    // and the browser makes of it what it makes of a person's Enter, which a
    // page is free to prevent.
    if(key->GetBool("enter")) {
      pressed->SetString("key","Enter");
      pressed->SetBool("down",true);Key(pressed);
      pressed->SetBool("down",false);Key(pressed);
      key_modifiers_=held;return;
    }
    pressed->SetString("key",key->GetString("text"));
    const std::string text=pressed->GetString("text").ToString();
    if(text.size()==1 && tilo::KeyCodeShaped("Key"+std::string(1,static_cast<char>(std::toupper(text[0])))))
      pressed->SetString("code","Key"+std::string(1,static_cast<char>(std::toupper(text[0]))));
    pressed->SetBool("down",true);
    Key(pressed);
    pressed->SetBool("down",false);Key(pressed);
    key_modifiers_=held;
  }
  CefRefPtr<CefBrowser> browser_;
  // This tab as extensions are told of it.
  tilo::ExtensionTab ExtensionState() const {
    tilo::ExtensionTab tab;tab.tab=id_;
    if(browser_) {tab.browser=tab.id=browser_->GetIdentifier();tab.url=browser_->GetMainFrame()->GetURL().ToString();tab.loading=browser_->IsLoading();tab.zoom=std::pow(1.2,browser_->GetHost()->GetZoomLevel());}
    tab.title=title_;tab.favicon=favicon_;tab.width=geometry_.logical_width();tab.height=geometry_.logical_height();
    tab.panel=extension_panel_;tab.opener=panel_opener_;return tab;
  }
  CefRefPtr<tilo::TabInspector> inspector() const {return inspector_;}
  // The page as this tab last painted it, for an extension that captures it.
  std::shared_ptr<tilo::ExtensionFrame> PaintedFrame() const {
    if(!visible_ || !frame_)return nullptr;
    auto frame=std::make_shared<tilo::ExtensionFrame>();frame->pixels.assign(*frame_,tilo::FramePixelOffset(static_cast<uint32_t>(id_)));frame->width=pixel_width_;frame->height=pixel_height_;return frame;
  }
  bool records_agent() const {return agent_log_ && agent_log_->active();}
  void LoadUrl(const std::string& url) {if(browser_)browser_->GetMainFrame()->LoadURL(url);}
  void ReloadPage(bool bypass) {if(!browser_)return;if(bypass)browser_->ReloadIgnoreCache();else browser_->Reload();}
  // What a gesture's drag may do here: a page reorders its own items with a
  // move, and a drop that copies leaves the original where it was. A page's
  // own drag offers both, so accepting only a move would refuse its drops.
  static constexpr cef_drag_operations_mask_t kPageDragOps=static_cast<cef_drag_operations_mask_t>(DRAG_OPERATION_MOVE|DRAG_OPERATION_COPY);
  // The page's own drag while a gesture carries it, and whether the element
  // under the pointer has heard it arrive.
  CefRefPtr<CefDragData> dragging_;bool dragging_entered_=false;
  bool detached() const {return transfer_owner_;}
  // The person clicked here or pressed a key within the last few seconds,
  // as long as Chromium counts it as their activation of the page.
  bool activated() const {return activated_ && std::chrono::steady_clock::now()-*activated_<=std::chrono::seconds(5);}
  bool accepts_downloads() const {return browser_ && !transfer_owner_ && !native_closing_;}
  void CloseNative() {if(!browser_ || native_closing_)return;InvalidateCredentials();native_closing_=true;CancelMenu();browser_->GetHost()->CloseDevTools();browser_->GetHost()->CloseBrowser(true);}
  void DetachVisible() {
    if(transfer_owner_ || native_closing_)return;const bool retained=downloads->Detach(id_);if(!retained){CloseNative();return;}
    // Every busy visible owner reserved this slot at admission, so detaching
    // cannot displace accepted work even while earlier owners await destruction.
    if(!download_owners.Detach(id_)){Event("close_blocked");return;}
    InvalidateCredentials();
    CancelMenu();
    transfer_owner_=true;visible_=false;auto host=browser_->GetHost();host->WasHidden(true);host->SetFocus(false);
    capabilities_->Close();frame_.reset();popup_.clear();whole_next_=true;Event("tab_detached");PublishDownloadRuntime();
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
  int id_;bool visible_=true,shown_for_action_=false,transfer_owner_=false,native_closing_=false,popups_=false;int opener_=0,key_modifiers_=0;uint64_t favicon_generation_=0;
  std::string title_,favicon_;bool extension_panel_=false;int panel_opener_=0;
  // The extension whose side panel this tab shows, if it shows one.
  std::string side_panel_;
  std::optional<std::chrono::steady_clock::time_point> activated_;
  // The paste a key is waiting on the system clipboard for, and the keys
  // pressed since (KeyCommand).
  int paste_prime_=0;std::deque<CefRefPtr<CefDictionaryValue>> held_keys_;bool shares_clipboard_=false;
  // The zoom level last told of (PublishZoom), none before the first page.
  std::optional<double> published_zoom_;
  CefRefPtr<tilo::TabInspector> inspector_;CefRefPtr<tilo::DocumentScripts> extension_scripts_;
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
  CefRefPtr<tilo::Geolocation> geolocation_;
  CefRefPtr<tilo::AgentLog> agent_log_;
  CefRefPtr<tilo::PaintSnapshot> paint_snapshot_;
  CefRefPtr<tilo::DocumentSize> document_size_;
  CefRefPtr<tilo::Capabilities> capabilities_;
  CefRefPtr<tilo::StartupDiagnostics> startup_diagnostics_;
  CefRefPtr<tilo::DocumentScripts> document_scripts_,composition_scripts_;std::vector<CefRefPtr<CefDictionaryValue>> pending_navigation_;
  CefRefPtr<tilo::SiteClear> site_clear_;
  CefRefPtr<tilo::ServiceSessionImport> service_session_import_;
  int pixel_width_=0,pixel_height_=0,popup_width_=0,popup_height_=0,mouse_modifiers_=0;
  // The press a following press may continue as a double or triple click.
  // The last focused-field and composition reports sent, so each goes out
  // only when it changes.
  std::string text_field_report_,composition_report_;
  bool text_field_editable_=false,text_field_probe_scheduled_=false;
  // The embedder last gave the page the keyboard (`focus`, `visibility`).
  bool page_focused_=false;
  int click_count_=0,click_x_=0,click_y_=0;cef_mouse_button_type_t click_button_=MBT_LEFT;std::chrono::steady_clock::time_point click_time_;
  // What of the wheel's last movement was less than a whole pixel.
  double wheel_rest_x_=0,wheel_rest_y_=0;
  // The pointer shape the page last asked for, by its CSS name.
  std::string cursor_;
  CefRefPtr<CefRunContextMenuCallback> context_callback_;int context_id_=0;std::set<int> context_commands_;
  // Keys pressed for an extension's command, whose release is the command's too.
  std::set<std::string> commanded_keys_;
  // The page's open question, if any, and whether the embedder answers them.
  static constexpr size_t kMaxDialogText=16*1024;
  CefRefPtr<CefJSDialogCallback> dialog_callback_;int dialog_id_=0;bool dialog_unload_=false,js_dialogs_=false;
  // The extensions' items in the open menu, by command from kFirstMenuOffer,
  // and where it opened.
  static constexpr int kFirstMenuOffer=27000;static constexpr size_t kMaxMenuOffers=200;
  std::vector<tilo::MenuOffer> menu_offers_;tilo::MenuContext menu_context_;
  bool finding_=false,select_initial_=false;CefString find_text_;int find_id_=0;std::deque<CefRefPtr<CefDictionaryValue>> pending_find_;
  tilo::ViewGeometry geometry_;bool stale_paint_recovery_pending_=false;bool popup_visible_=false;CefRect popup_rect_;std::shared_ptr<const std::string> frame_;
  // The next frame goes whole: the app may not have the one before it.
  bool whole_next_=true;std::string popup_;std::vector<CefString> scripts_,blocking_scripts_;
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
  // A sealed document runs no script, whatever the document says.
  if(sealed_documents) settings.javascript=STATE_DISABLED;
  TraceGeometry("create-begin",id);
  if(!CefBrowserHost::CreateBrowserSync(window,client,"about:blank",settings,nullptr,shared_context)) {clients.erase(id);CloseHost();}
  TraceGeometry("create-return",id);
}
void Dispatch(CefRefPtr<CefDictionaryValue> d) {
  const auto op=d->GetString("op");int id=d->HasKey("tab")?d->GetInt("tab"):1;
  if(op=="shutdown") {CloseHost();return;}
  if(op=="create_tab") {CreateTab(id);return;}
  if(op=="devtools_call") {DevToolsCall(d,id);return;}
  // The system clipboard's text, for whichever tab asked (page_clipboard.h).
  if(op=="clipboard_contents") {ClipboardAnswer(d->GetInt("request_id"),d->GetType("text")==VTYPE_STRING?std::optional<std::string>(d->GetString("text").ToString()):std::nullopt);return;}
  if(op=="extension_access") {
    std::set<std::string> permissions;std::vector<std::string> hosts,scriptable,framed;
    auto list=d->GetList("permissions");for(size_t i=0;i<list->GetSize();++i)permissions.insert(list->GetString(i));
    list=d->GetList("hosts");for(size_t i=0;i<list->GetSize();++i)hosts.push_back(list->GetString(i));
    // Where its content scripts run, apart from its host permissions.
    list=d->GetList("scriptable");for(size_t i=0;list && i<list->GetSize();++i)scriptable.push_back(list->GetString(i));
    // Those of them that run in a page's frames too (all_frames).
    list=d->GetList("framed");for(size_t i=0;list && i<list->GetSize();++i)framed.push_back(list->GetString(i));
    if(extension_tabs)extension_tabs->SetAccess(d->GetString("id"),std::move(permissions),std::move(hosts),std::move(scriptable),std::move(framed),d->HasKey("token")?d->GetString("token").ToString():std::string(),
      d->GetType("commands")==VTYPE_DICTIONARY?d->GetDictionary("commands"):nullptr);
    return;
  }
  // What an extension may ask for later, and what of it the person granted,
  // as the Browser keeps it (extension_permissions.h).
  if(op=="extension_grants") {
    auto set=[&](const char* key) {
      tilo::PermissionSet read;auto inner=d->GetDictionary(key);
      for(const char* name:{"permissions","hosts"}) {
        auto list=inner->GetList(name);auto& into=std::string(name)=="permissions"?read.permissions:read.origins;
        for(size_t i=0;list && i<list->GetSize();++i)into.push_back(list->GetString(i));
      }
      return read;
    };
    if(extension_tabs)extension_tabs->SetOptional(d->GetString("id"),set("optional"),set("granted"));
    return;
  }
  // The person answered an extension's request for more access.
  if(op=="extension_permission_decision") {if(extension_tabs)extension_tabs->PermissionDecision(d->GetInt("request"),d->GetBool("allow"));return;}
  if(op=="extension_forget") {if(extension_tabs)extension_tabs->Forget(d->GetString("id"));return;}
  // The person chose an extension's notification, or one of its buttons.
  if(op=="extension_notification_chosen") {
    if(extension_tabs)extension_tabs->NotificationChosen(d->GetString("id"),d->GetString("notification"),d->HasKey("button")?std::optional<int>(d->GetInt("button")):std::nullopt);
    return;
  }
  // The Browser's own extension, which tells which tab a page is.
  if(op=="extension_identity") {if(extension_tabs)extension_tabs->SetIdentity(d->GetString("id"));return;}
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
// Privacy defaults nobody should have to find a setting for, as Safari and
// Firefox ship them: a site embedded in another (an ad, a tracker's frame or
// pixel) gets no cookies or storage of its own there, and WebRTC tells a page
// only the public address the network already sees, never the device's
// local ones. Both are profile preferences, so a private window's context
// gets them too.
void ApplyPrivacyDefaults(CefRefPtr<CefRequestContext> context) {
  auto set=[&context](const char* name,CefRefPtr<CefValue> value) {
    CefString error;
    if(!context->SetPreference(name,value,error))
      std::cerr<<"Could not set the privacy default "<<name<<": "<<error.ToString()<<"\n";
  };
  // content_settings::CookieControlsMode::kBlockThirdParty.
  auto cookies=CefValue::Create();cookies->SetInt(1);
  set("profile.cookie_controls_mode",cookies);
  auto webrtc=CefValue::Create();webrtc->SetString("default_public_interface_only");
  set("webrtc.ip_handling_policy",webrtc);
  if(sealed_documents) {
    // chrome_browser_net::NETWORK_PREDICTION_NEVER: no DNS or preconnect
    // for a sealed document's links.
    auto prediction=CefValue::Create();prediction->SetInt(2);
    set("net.network_prediction_options",prediction);
  }
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
#if defined(OS_MAC)
    // Chromium keeps the key that encrypts cookies and saved site data in the
    // login keychain ("Chromium Safe Storage"), and the keychain grants that
    // item to one code signature. Every development build is signed ad hoc,
    // so each rebuilt engine asks the person again, and until someone answers
    // the network service (in this process) waits and no page loads. Without
    // a team identity the keychain cannot hold a grant across builds: keep the
    // key out of it, as Tilo's own identity keeps an unsigned build's secrets
    // in owner-only profile files instead. A signed engine keeps the keychain.
    if(process_type.empty() && !HostHasStableSigningIdentity() && !command_line->HasSwitch("use-mock-keychain"))
      command_line->AppendSwitch("use-mock-keychain");
#endif
#if defined(OS_LINUX)
    // Every frame is rendered offscreen, yet Chromium's default Linux platform
    // exits unless it reaches an X server, and a Tilo app sandbox has none.
    if(process_type.empty() && !std::getenv("DISPLAY") && !command_line->HasSwitch("ozone-platform"))
      command_line->AppendSwitchWithValue("ozone-platform","headless");
    // There Chromium's clipboard is its own alone: the host bridges it to
    // the Browser's (page_clipboard.h).
    if(process_type.empty() && command_line->GetSwitchValue("ozone-platform")=="headless") page_clipboard=true;
    // Without a GPU (a hosting server running agents), in-process GPU work
    // fails EGL and takes the browser down: compose in software instead, and
    // trade caches for memory as Chromium does on low-end devices.
    if(process_type.empty() && !HasRenderNode()) {
      for(const char* flag:{"disable-gpu","enable-low-end-device-mode"})
        if(!command_line->HasSwitch(flag)) command_line->AppendSwitch(flag);
    }
#endif
  }
  // Each renderer runs the page clipboard where the engine has none.
  void OnBeforeChildProcessLaunch(CefRefPtr<CefCommandLine> command_line) override {
    if(page_clipboard && !sealed_documents && command_line->GetSwitchValue("type")=="renderer") command_line->AppendSwitch(tilo::kPageClipboardSwitch);
  }
  void OnContextInitialized() override {
    request_filter=new tilo::RequestFilter([] { StringEvent("error","message","Native request matcher failed to check a resource"); },sealed_documents);
    // Where each extension's files are, as Chromium says, for an ad
    // blocker's stand-ins at its address for the session.
    request_filter->ServeStandIns(std::make_shared<tilo::StandIns>(std::make_shared<tilo::ExtensionDirectories>([](tilo::ExtensionDirectories::Answer answer) {
      PipeCall("Extensions.getExtensions",nullptr,"",[answer](bool ok,CefRefPtr<CefDictionaryValue> result) {
        std::map<std::string,std::string> directories;
        auto list=ok && result && result->GetType("extensions")==VTYPE_LIST?result->GetList("extensions"):nullptr;
        for(size_t i=0;list && i<list->GetSize();++i) {
          auto extension=list->GetDictionary(i);
          if(extension && extension->GetType("id")==VTYPE_STRING && extension->GetType("path")==VTYPE_STRING) directories[extension->GetString("id")]=extension->GetString("path");
        }
        answer(std::move(directories));
      });
    })));
    if(private_mode) {
      CefRequestContextSettings settings;
      // Empty cache_path is CEF's incognito request context. The separate
      // root_cache_path below still gives CEF an owned installation-data root.
      shared_context=CefRequestContext::CreateContext(settings,request_filter);
    } else {
      shared_context=CefRequestContext::CreateContext(CefRequestContext::GetGlobalContext(),request_filter);
    }
    ApplyPrivacyDefaults(shared_context);
    StartExtensionTabs();
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
CefRefPtr<Client> ClientOf(int tab) {auto found=clients.find(tab);return found==clients.end()?nullptr:found->second;}
// The Browser's answer to a read of the system clipboard, or nothing when it
// had none or did not answer in time.
void ClipboardAnswer(int request,std::optional<std::string> text) {
  auto read=clipboard_reads.Take(request);if(!read)return;
  if(auto client=ClientOf(read->tab))client->ClipboardRead(*read,std::move(text));
}
void StartExtensionTabs() {
  tilo::ExtensionTabs::Hooks hooks;
  hooks.tabs=[] {
    std::vector<tilo::ExtensionTab> tabs;
    for(const auto& [id,client]:clients)if(client->browser_ && !client->detached())tabs.push_back(client->ExtensionState());
    return tabs;
  };
  hooks.focused=[] {return last_focused_tab;};
  hooks.inspector=[](int tab) {auto client=ClientOf(tab);return client?client->inspector():nullptr;};
  hooks.keeps_contexts=[](int tab) {auto client=ClientOf(tab);return client && client->records_agent();};
  hooks.navigate=[](int tab,const std::string& url) {if(auto client=ClientOf(tab))client->LoadUrl(url);};
  hooks.reload=[](int tab,bool bypass) {if(auto client=ClientOf(tab))client->ReloadPage(bypass);};
  hooks.open=[](int tab,const std::string& url,bool background) {
    auto client=ClientOf(tab);
    if(!client && !clients.empty())client=clients.begin()->second;
    if(client)client->OpenTab(url,background);
  };
  hooks.close=[](int tab) {if(auto client=ClientOf(tab))client->CloseNative();};
  hooks.frame=[](int tab)->std::shared_ptr<tilo::ExtensionFrame> {auto client=ClientOf(tab);return client?client->PaintedFrame():nullptr;};
  hooks.pipe=&PipeCall;
  hooks.event=[](int tab,const std::string& name,CefRefPtr<CefDictionaryValue> fields) {if(auto client=ClientOf(tab))client->Event(name,fields);};
  // An extension's download belongs to a page, as every download here
  // does: the one it was asked on, else any the person can see.
  hooks.download=[](int tab,const std::string& url,const std::string& name,tilo::Downloads::Started done) {
    auto client=ClientOf(tab);
    if(client && !client->accepts_downloads()) {client=nullptr;tab=0;}
    for(const auto& [id,other]:clients)if(!client && other->accepts_downloads() && !other->ExtensionState().panel) {client=other;tab=id;}
    if(!downloads || !client) {done(0,"No page to download from");return;}
    downloads->Expect(tab,url,name,std::move(done));
    client->browser_->GetHost()->StartDownload(url);
  };
  hooks.open_download=[](int tab,int id,bool reveal)->std::string {
    if(!downloads || !downloads->Known(id))return "Invalid downloadId";
    if(downloads->CompletedPath(id).empty())return "Download must be complete";
    auto client=ClientOf(tab);
    if(!client)return "No window to open it in";
    auto fields=CefDictionaryValue::Create();fields->SetInt("id",id);fields->SetBool("reveal",reveal);
    client->Event("extension_download_open",fields);
    return "";
  };
  // The person's click or key lands in the focused frame, and Chromium
  // counts it for that frame and those it is in: one of them must be the
  // extension's page.
  hooks.activated=[](int tab,const std::string& extension) {
    auto client=ClientOf(tab);if(!client || !client->activated() || !client->browser_)return false;
    const auto page="chrome-extension://"+extension+"/";
    for(auto frame=client->browser_->GetFocusedFrame();frame;frame=frame->GetParent())
      if(frame->GetURL().ToString().rfind(page,0)==0)return true;
    return false;
  };
  hooks.pipe_listen=[](const std::string& session,tilo::ExtensionTabs::PipeListener listener) {
    if(listener)pipe_listeners[session]=std::move(listener);else pipe_listeners.erase(session);
  };
  // What an extension's offscreen document copied goes to the Browser by
  // the page the person used last, as the Browser shows it.
  if(page_clipboard) hooks.clipboard=[](CefRefPtr<CefDictionaryValue> copy) {
    auto client=ClientOf(last_focused_tab);
    for(const auto& [id,other]:clients)if(!client && !other->ExtensionState().panel)client=other;
    if(client && client->SharesClipboard())client->ClipboardOwner()->Event("clipboard",copy);
  };
  hooks.private_mode=private_mode;
  if(!private_mode && !extension_menus_file.empty())
    hooks.save_menus=[](std::string json) {
      // Written whole, then put in place: a crash leaves the old file.
      const std::string partial=extension_menus_file+".partial";
      {std::ofstream out(partial,std::ios::binary|std::ios::trunc);out<<json;if(!out.good()) return;}
      std::error_code error;std::filesystem::rename(partial,extension_menus_file,error);
    };
  extension_tabs=std::make_unique<tilo::ExtensionTabs>(std::move(hooks));
  if(!private_mode && !extension_menus_file.empty()) {
    std::ifstream in(extension_menus_file,std::ios::binary);
    std::string json((std::istreambuf_iterator<char>(in)),std::istreambuf_iterator<char>());
    if(!json.empty() && json.size()<=16*1024*1024) extension_tabs->LoadMenus(json);
  }
  CefRefPtr<tilo::ExtensionBridgeFactory> bridge=new tilo::ExtensionBridgeFactory(
    [](int browser,const std::string& frame,const std::string& method,const std::string& body,tilo::ExtensionTabs::Reply reply) {
      int caller=0;
      for(const auto& [id,client]:clients)if(browser && client->browser_ && client->browser_->GetIdentifier()==browser)caller=id;
      // A page's request from no tab of the host's is no extension's.
      if(browser && !caller) {reply(R"({"error":"Not an extension"})");return;}
      if(extension_tabs)extension_tabs->Request(caller,frame,method,body,std::move(reply));
    });
  for(auto context:{CefRequestContext::GetGlobalContext(),shared_context})
    if(context)context->RegisterSchemeHandlerFactory("https","extension-bridge.tilo.invalid",bridge);
}
// A command is judged before it reaches a tab, so a page request it refuses
// is handed to that tab's automation where the page's own answers come from:
// behind whatever is still running there, in the order the caller asked. The
// tab's own dispatch is already on its way, so the refusal is posted behind it
// and the tab answers the request it was never given.
void RefuseOnThread(CefRefPtr<CefDictionaryValue> d,const std::string& message) {
  // A command without a tab names the first one, as the rest of the host
  // reads it.
  const int tab=d->GetType("tab")==VTYPE_INT?d->GetInt("tab"):1;
  CefPostTask(TID_UI,base::BindOnce([](int tab,CefRefPtr<CefDictionaryValue> d,std::string message) {
    auto found=clients.find(tab);
    if(found==clients.end()) return;
    found->second->RefusePage(d,message);
  },tab,d,message));
}
bool ValidCommand(CefRefPtr<CefDictionaryValue> d) {
  if(d->HasKey("tab") && (d->GetType("tab")!=VTYPE_INT || d->GetInt("tab")<=0)) return false;
  auto op=d->GetString("op").ToString();
  auto type=[&](const char* key,cef_value_type_t expected){return d->GetType(key)==expected;};
  auto number=[&](const char* key){auto t=d->GetType(key);return t==VTYPE_INT || (t==VTYPE_DOUBLE && std::isfinite(d->GetDouble(key)));};
  auto number_in=[&](CefRefPtr<CefDictionaryValue> value,const char* key) {
    auto t=value->GetType(key);if(t!=VTYPE_INT && t!=VTYPE_DOUBLE)return false;
    const double n=t==VTYPE_DOUBLE?value->GetDouble(key):static_cast<double>(value->GetInt(key));
    return std::isfinite(n) && n>=0 && n<=100000;
  };
  auto no_payload=[&]{CefDictionaryValue::KeyList keys;d->GetKeys(keys);return std::all_of(keys.begin(),keys.end(),[](const CefString& key){return key=="op" || key=="tab";});};
  if(op=="create_tab" || op=="close_tab" || op=="try_close_tab") return d->HasKey("tab");
  if(op=="visibility") return type("visible",VTYPE_BOOL);
  if(op=="resend_frame") return d->HasKey("tab") && no_payload();
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
  if(op=="js_dialogs") return type("enabled",VTYPE_BOOL);
  if(op=="js_dialog_reply") return type("id",VTYPE_INT) && d->GetInt("id")>0 && type("accept",VTYPE_BOOL) && (!d->HasKey("text") || (type("text",VTYPE_STRING) && d->GetString("text").ToString().size()<=64*1024));
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
  if(op=="key") return type("key",VTYPE_STRING) && type("down",VTYPE_BOOL) &&
    (!d->HasKey("code") || (type("code",VTYPE_STRING) && tilo::KeyCodeShaped(d->GetString("code").ToString())));
  if(op=="ime_composition") {
    if(!type("text",VTYPE_STRING) || d->GetString("text").ToString().size()>4096 || !type("selection_start",VTYPE_INT) || !type("selection_end",VTYPE_INT))return false;
    const auto length=static_cast<int>(d->GetString("text").ToString16().size());
    const int start=d->GetInt("selection_start"),end=d->GetInt("selection_end");
    return 0<=start && start<=end && end<=length;
  }
  if(op=="ime_commit") return type("text",VTYPE_STRING) && !d->GetString("text").empty() && d->GetString("text").ToString().size()<=64*1024;
  if(op=="ime_cancel" || op=="ime_finish") return no_payload();
  if(op=="scripts" || op=="blocking_scripts") {
    if(!type("scripts",VTYPE_LIST)) return false;
    auto list=d->GetList("scripts");for(size_t i=0;i<list->GetSize();++i) if(list->GetType(i)!=VTYPE_STRING) return false;
    return true;
  }
  if(op=="site_exceptions") {if(!type("origins",VTYPE_LIST))return false;auto list=d->GetList("origins");if(list->GetSize()>1024)return false;for(size_t i=0;i<list->GetSize();++i){if(list->GetType(i)!=VTYPE_STRING)return false;const auto origin=list->GetString(i).ToString();if(origin.empty() || tilo::Origin(origin)!=origin)return false;}return true;}
  if(op=="service_session_import")return tilo::ServiceSessionCommandValid(d);
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
    const auto action=d->GetString("action");return number("generation") && tilo::ValidDocumentGeneration(Number(d,"generation")) && (action=="clear" || action=="revoke" || action=="reset" || action=="enable" || action=="disable" || action=="reload");
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
    if(action!="outline" && action!="click" && action!="fill" && action!="select" && action!="fill_files" && action!="press" && action!="submit" &&
       action!="changes" && action!="at" && action!="settle" && action!="console" && action!="network" &&
       action!="reader" && action!="pointer" && action!="hover" && action!="drag" && action!="path" &&
       action!="scroll" && action!="pointer_rest" && action!="capture")return false;
    const bool elementless=action=="outline" || action=="press" || action=="changes" || action=="at" || action=="settle" ||
      action=="console" || action=="network" || action=="reader" || action=="pointer" || action=="hover" ||
      action=="drag" || action=="path" || action=="scroll" || action=="pointer_rest" || action=="capture";
    if(action=="pointer") {
      if(!type("steps",VTYPE_LIST) || !type("button",VTYPE_STRING))return false;
      const auto button=d->GetString("button").ToString();
      if(button!="left" && button!="right")return false;
      auto steps=d->GetList("steps");
      if(!steps->GetSize() || steps->GetSize()>256)return false;
      for(size_t i=0;i<steps->GetSize();++i) {
        auto step=steps->GetDictionary(i);
        if(!step || step->GetType("kind")!=VTYPE_STRING || !number_in(step,"x") || !number_in(step,"y"))return false;
        const auto kind=step->GetString("kind").ToString();
        if(kind!="move" && kind!="down" && kind!="up")return false;
        if(step->HasKey("element") && (step->GetType("element")!=VTYPE_STRING || step->GetString("element").empty() || step->GetString("element").length()>64))return false;
        // A step names a frame the outline printed, or none at all.
        if(step->HasKey("frame") && (step->GetType("frame")!=VTYPE_STRING || step->GetString("frame").empty() || step->GetString("frame").length()>16))return false;
      }
    }
    if((action=="reader")!=d->HasKey("enabled") || (d->HasKey("enabled") && !type("enabled",VTYPE_BOOL)))return false;
    if(d->HasKey("element") ? !type("element",VTYPE_STRING) || d->GetString("element").empty() || d->GetString("element").length()>64
                            : !elementless)return false;
    for(const char* flag:{"cross_origin","secret"})if(d->HasKey(flag) && !type(flag,VTYPE_BOOL))return false;
    // A frame is named as the outline printed it, or not at all.
    if(d->HasKey("frame") && (!type("frame",VTYPE_STRING) || d->GetString("frame").empty() || d->GetString("frame").length()>16))return false;
    if(action=="at" && (!number("x") || !number("y") || Number(d,"x")<0 || Number(d,"y")<0 || Number(d,"x")>100000 || Number(d,"y")>100000))return false;
    if(action=="settle" && (!type("quiet_ms",VTYPE_INT) || !type("timeout_ms",VTYPE_INT) || d->GetInt("quiet_ms")<50 ||
       d->GetInt("timeout_ms")<d->GetInt("quiet_ms") || d->GetInt("timeout_ms")>20000))return false;
    if((action=="console" || action=="network") && (!number("since") || Number(d,"since")<0))return false;
    // A picture is painted at most as wide as the app asks and no wider
    // than the engine's own bound; a wider one is refused, not cut.
    if(action=="capture" && d->HasKey("max_width") &&
       (!type("max_width",VTYPE_INT) || d->GetInt("max_width")<1 || d->GetInt("max_width")>CAPTURE_MAX_WIDTH))return false;
    if(action=="fill" && (!type("text",VTYPE_STRING) || d->GetString("text").length()>65536))return false;
    if(action=="select") {
      if(!type("values",VTYPE_LIST))return false;auto values=d->GetList("values");
      if(!values->GetSize() || values->GetSize()>64)return false;
      for(size_t i=0;i<values->GetSize();++i)if(values->GetType(i)!=VTYPE_STRING || values->GetString(i).length()>1024)return false;
    }
    if(action=="fill_files") {
      if(!type("paths",VTYPE_LIST))return false;auto paths=d->GetList("paths");
      if(!paths->GetSize() || paths->GetSize()>16)return false;
      for(size_t i=0;i<paths->GetSize();++i) {
        if(paths->GetType(i)!=VTYPE_STRING)return false;
        const auto path=paths->GetString(i).ToString();
        // The app checks that each path is a readable regular file; the
        // engine only insists it is absolute, so no relative path is ever
        // resolved against the browser's own working folder.
        if(path.empty() || path.size()>4096 || path.front()!='/')return false;
      }
    }
    if(action=="press") {
      if(!type("key",VTYPE_STRING) || d->GetString("key").empty() || d->GetString("key").length()>16)return false;
      for(const char* flag:{"ctrl","shift","alt","meta"})if(!type(flag,VTYPE_BOOL))return false;
    }
    // Where the engine's own pointer came to rest, so the page's hover
    // styles follow it; the engine sends it, the action door never does.
    if(action=="pointer_rest") return (d->HasKey("x") && d->HasKey("y"));
    if(action=="hover" || action=="drag") {
      // One end of a gesture either names an element or gives its point.
      if(d->HasKey("x")!=d->HasKey("y") || d->HasKey("to_x")!=d->HasKey("to_y"))return false;
      const auto point=[&](const char* x,const char* y) {
        if(!d->HasKey(x))return true;
        return number(x) && number(y) && Number(d,x)>=0 && Number(d,y)>=0 && Number(d,x)<=100000 && Number(d,y)<=100000;
      };
      if(!point("x","y") || !point("to_x","to_y"))return false;
      if(action=="hover" && !d->HasKey("element") && !d->HasKey("x"))return false;
      if(action=="drag" && (!d->HasKey("element") && !d->HasKey("x") || !d->HasKey("to") && !d->HasKey("to_x")))return false;
      if(d->HasKey("element") && d->HasKey("x") || d->HasKey("to") && d->HasKey("to_x"))return false;
      if(d->HasKey("to") && (!type("to",VTYPE_STRING) || d->GetString("to").empty() || d->GetString("to").length()>64))return false;
    }
    if(action=="drag" && (!type("steps",VTYPE_INT) || d->GetInt("steps")<1 || d->GetInt("steps")>64))return false;
    if(action=="scroll") {
      // Where the wheel turns, and where it goes: one of `to`, `by` or
      // `into_view`, each bounded so one request stays bounded work. A
      // request outside those bounds is answered as failed, not dropped.
      // The refusal is queued like any other page request, and the page's
      // own actions behind it wait on it: a refused scroll keeps the caller's
      // order, so the action after it runs after it.
      const auto refuse=[&](const char* message) {
        RefuseOnThread(d,message);
        return true;
      };
      if(d->HasKey("to") && (!type("to",VTYPE_STRING) || (d->GetString("to")!="top" && d->GetString("to")!="bottom")))
        return refuse("A scroll goes to the top or the bottom of the page");
      // As far as one scroll may ask to travel at once, which the SDK also
      // refuses beyond.
      static constexpr int kMaxScrollBy=10000;
      if(d->HasKey("by") && (!type("by",VTYPE_INT) || d->GetInt("by")<-kMaxScrollBy || d->GetInt("by")>kMaxScrollBy))
        return refuse("A scroll by a distance stays within 10000 pixels");
      if(d->HasKey("into_view") && (!type("into_view",VTYPE_STRING) || d->GetString("into_view").empty() || d->GetString("into_view").length()>64))
        return refuse("Bringing an element into view names one ref");
      // The wheel turns over the viewport's centre unless the caller names
      // what it turns over, and it turns until exactly one place is reached:
      // the top or bottom, a distance, or an element brought into view.
      const int places=(d->HasKey("to")?1:0)+(d->HasKey("by")?1:0);
      if(d->HasKey("into_view") ? places!=0 : places!=1)
        return refuse("A scroll says where it goes once, and how far");
    }
    if(action=="path") {
      if(!type("points",VTYPE_LIST))return false;
      auto points=d->GetList("points");
      if(points->GetSize()<2 || points->GetSize()>1024)return false;
      const bool relative=d->HasKey("element");
      for(size_t i=0;i<points->GetSize();++i) {
        auto pair=points->GetType(i)==VTYPE_LIST?points->GetList(i):nullptr;
        auto scalar=[](CefRefPtr<CefValue> v){return v->GetType()==VTYPE_DOUBLE || v->GetType()==VTYPE_INT;};
        if(!pair || pair->GetSize()!=2 || !scalar(pair->GetValue(0)) || !scalar(pair->GetValue(1)))return false;
        const double x=pair->GetDouble(0),y=pair->GetDouble(1);
        if(!std::isfinite(x) || !std::isfinite(y))return false;
        if(relative ? std::abs(x)>100000 || std::abs(y)>100000 : x<0 || y<0 || x>100000 || y>100000)return false;
      }
    }
    return true;
  }
  if(op=="paint_capture") {
    if(!type("request_id",VTYPE_INT) || d->GetInt("request_id")<=0 || !type("styles",VTYPE_LIST))return false;
    auto styles=d->GetList("styles");if(!styles->GetSize() || styles->GetSize()>64)return false;
    for(size_t i=0;i<styles->GetSize();++i)if(styles->GetType(i)!=VTYPE_STRING || styles->GetString(i).empty() || styles->GetString(i).length()>64)return false;
    return true;
  }
  if(op=="selection_text") {
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="request_id")return false;
    return type("request_id",VTYPE_INT) && d->GetInt("request_id")>0;
  }
  if(op=="popups") return type("enabled",VTYPE_BOOL);
  if(op=="page_clipboard") return no_payload();
  if(op=="clipboard_contents") {
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="request_id" && key!="text")return false;
    return type("request_id",VTYPE_INT) && d->GetInt("request_id")>0 &&
      (!d->HasKey("text") || (type("text",VTYPE_STRING) && d->GetString("text").ToString().size()<=tilo::kMaxClipboardText));
  }
  if(op=="extension_panel") {
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="action" && key!="side_panel")return false;
    // Opened by its button, or a side panel; not both.
    if(d->HasKey("action") && d->HasKey("side_panel"))return false;
    return (!d->HasKey("action") || (type("action",VTYPE_STRING) && tilo::ExtensionId(d->GetString("action")))) &&
           (!d->HasKey("side_panel") || (type("side_panel",VTYPE_STRING) && tilo::ExtensionId(d->GetString("side_panel"))));
  }
  if(op=="extension_command_key") {
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="key" && key!="ctrl" && key!="shift" && key!="alt" && key!="meta")return false;
    if(!type("key",VTYPE_STRING) || d->GetString("key").empty() || d->GetString("key").length()>16)return false;
    for(const char* flag:{"ctrl","shift","alt","meta"})if(!type(flag,VTYPE_BOOL))return false;
    return true;
  }
  if(op=="extension_click" || op=="extension_side_panel" || op=="extension_forget" || op=="extension_identity") return type("id",VTYPE_STRING) && tilo::ExtensionId(d->GetString("id"));
  if(op=="extension_menu_action") return type("id",VTYPE_STRING) && tilo::ExtensionId(d->GetString("id")) && type("key",VTYPE_STRING) && !d->GetString("key").empty() && d->GetString("key").length()<=tilo::ExtensionMenus::kMaxId;
  if(op=="extension_notification_chosen") {
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="id" && key!="notification" && key!="button")return false;
    return type("id",VTYPE_STRING) && tilo::ExtensionId(d->GetString("id")) && type("notification",VTYPE_STRING) &&
           d->GetString("notification").length()<=tilo::ExtensionNotifications::kMaxId &&
           (!d->HasKey("button") || (type("button",VTYPE_INT) && d->GetInt("button")>=0 && d->GetInt("button")<(int)tilo::ExtensionNotifications::kMaxButtons));
  }
  if(op=="extension_grants") {
    // Two sets, each of permissions and site patterns, as extension_access names them.
    auto set=[&](const char* key) {
      if(!type(key,VTYPE_DICTIONARY))return false;auto inner=d->GetDictionary(key);
      for(const char* name:{"permissions","hosts"}) {
        if(!inner->HasKey(name))continue;
        if(inner->GetType(name)!=VTYPE_LIST)return false;auto list=inner->GetList(name);if(list->GetSize()>512)return false;
        for(size_t i=0;i<list->GetSize();++i)if(list->GetType(i)!=VTYPE_STRING || list->GetString(i).length()>2048)return false;
      }
      return true;
    };
    return type("id",VTYPE_STRING) && tilo::ExtensionId(d->GetString("id")) && set("optional") && set("granted");
  }
  if(op=="extension_permission_decision") return type("request",VTYPE_INT) && d->GetInt("request")>0 && type("allow",VTYPE_BOOL);
  if(op=="extension_sign_in_panel") {
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="id" && key!="flow")return false;
    return type("id",VTYPE_STRING) && tilo::ExtensionId(d->GetString("id")) && type("flow",VTYPE_INT) && d->GetInt("flow")>0;
  }
  if(op=="extension_omnibox") {
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="id" && key!="input" && key!="text" && key!="request")return false;
    if(!type("id",VTYPE_STRING) || !tilo::ExtensionId(d->GetString("id")) || !type("input",VTYPE_STRING))return false;
    const auto input=d->GetString("input").ToString();
    // Typed text comes with what was typed; a change also with its request.
    const bool typed=input=="changed" || input=="entered";
    if(!typed && input!="started" && input!="cancelled")return false;
    if(typed != d->HasKey("text") || (input=="changed") != d->HasKey("request"))return false;
    if(typed && (!type("text",VTYPE_STRING) || d->GetString("text").length()>tilo::kMaxOmniboxText))return false;
    return input!="changed" || (type("request",VTYPE_INT) && d->GetInt("request")>=0);
  }
  if(op=="extension_access") {
    auto strings=[&](const char* key){
      if(!type(key,VTYPE_LIST))return false;auto list=d->GetList(key);if(list->GetSize()>512)return false;
      for(size_t i=0;i<list->GetSize();++i)if(list->GetType(i)!=VTYPE_STRING || list->GetString(i).length()>2048)return false;
      return true;
    };
    // The token the extension's worker shows the bridge (extension_tabs.h).
    const bool token=!d->HasKey("token") || (type("token",VTYPE_STRING) && d->GetString("token").length()>=32 && d->GetString("token").length()<=128);
    // Its keyboard shortcuts, as its manifest declares them (extension_commands.h).
    const bool commands=!d->HasKey("commands") || type("commands",VTYPE_DICTIONARY);
    return type("id",VTYPE_STRING) && tilo::ExtensionId(d->GetString("id")) && strings("permissions") && strings("hosts") && (!d->HasKey("scriptable") || strings("scriptable")) && (!d->HasKey("framed") || strings("framed")) && token && commands;
  }
  if(op=="document_size") {
    CefDictionaryValue::KeyList keys;d->GetKeys(keys);
    for(const auto& key:keys)if(key!="op" && key!="tab" && key!="enabled")return false;
    return type("enabled",VTYPE_BOOL);
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
  if(!WritePipe(std::move(text))) {devtools_pending.Forget(*id);DevToolsReply(tab,request_id,false,"Extension management is unavailable");}
}
// One NUL-terminated message on the DevTools pipe.
bool WritePipe(std::string text) {
  text.push_back('\0');
  // The reply thread never blocks this write: Chromium drains the pipe.
  size_t done=0;
  while(done<text.size()) {
    ssize_t n=write(devtools_write,text.data()+done,text.size()-done);
    if(n<0 && errno==EINTR) continue;
    if(n<=0) return false;
    done+=size_t(n);
  }
  return true;
}
void PipeCall(const std::string& method,CefRefPtr<CefDictionaryValue> params,const std::string& session,tilo::ExtensionTabs::PipeDone done) {
  auto fail=[](tilo::ExtensionTabs::PipeDone done){CefPostTask(TID_UI,base::BindOnce([](tilo::ExtensionTabs::PipeDone done){done(false,nullptr);},std::move(done)));};
  if(devtools_write<0) {fail(std::move(done));return;}
  const int handle=next_pipe_call=next_pipe_call%2147483647+1;
  auto id=devtools_pending.AddInternal(handle);
  if(!id) {fail(std::move(done));return;}
  auto message=CefDictionaryValue::Create();
  message->SetInt("id",*id);message->SetString("method",method);
  if(params) message->SetDictionary("params",params);
  if(!session.empty()) message->SetString("sessionId",session);
  auto value=CefValue::Create();value->SetDictionary(message);
  std::string text=CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString();
  if(text.size()>tilo::kMaxPipeMessage || !WritePipe(std::move(text))) {devtools_pending.Forget(*id);fail(std::move(done));return;}
  pipe_calls[handle]=std::move(done);
}
void PipeContext(const std::string& session,const std::string& json) {
  auto found=pipe_listeners.find(session);
  if(found==pipe_listeners.end()) return;
  auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
  if(parsed && parsed->GetType()==VTYPE_DICTIONARY) found->second(parsed->GetDictionary());
}
void PipeResult(int handle,bool ok,const std::string& json) {
  auto found=pipe_calls.find(handle);
  if(found==pipe_calls.end()) return;
  auto done=std::move(found->second);pipe_calls.erase(found);
  auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
  done(ok,parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr);
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
        // Notifications carry no id; only commands this host sent are
        // answered, and only the contexts of a session it attached heard.
        if(reply->GetType("id")!=VTYPE_INT) {
          if(reply->GetType("sessionId")==VTYPE_STRING && reply->GetString("method")=="Runtime.executionContextCreated" && reply->GetType("params")==VTYPE_DICTIONARY) {
            auto context=reply->GetDictionary("params")->GetDictionary("context");
            if(context) {auto value=CefValue::Create();value->SetDictionary(context->Copy(false));CefPostTask(TID_UI,base::BindOnce(&PipeContext,reply->GetString("sessionId").ToString(),CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString()));}
          }
          continue;
        }
        auto owner=devtools_pending.Take(reply->GetInt("id"));if(!owner) continue;
        const bool ok=reply->HasKey("result");
        auto payload=ok ? reply->GetValue("result") : reply->GetValue("error");
        if(owner->internal) {
          CefPostTask(TID_UI,base::BindOnce(&PipeResult,owner->internal,ok,payload ? CefWriteJSON(payload,JSON_WRITER_DEFAULT).ToString() : std::string("{}")));
          continue;
        }
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
    ssize_t n=read(STDIN_FILENO,chunk,sizeof(chunk));if(n<=0) break;buffer.append(chunk,n);{volatile char* secret=chunk;for(ssize_t i=0;i<n;++i)secret[i]=0;}
    bool bad=false;
    while(buffer.size()>=4) {
      auto size=tilo::U32(buffer.data());if(!size || size>tilo::kMaxPacket) {bad=true;break;}
      if(buffer.size()<size_t(size)+4) break;
      if(buffer[4]!=1) {bad=true;break;}
      std::string payload=buffer.substr(5,size-1);
      auto parsed=CefParseJSON(payload,JSON_PARSER_RFC);tilo::WipeServiceTransport(payload);
      if(!parsed || parsed->GetType()!=VTYPE_DICTIONARY || parsed->GetDictionary()->GetType("op")!=VTYPE_STRING) {bad=true;break;}
      auto d=parsed->GetDictionary();if(!ValidCommand(d)) {if(d->GetString("op")=="service_session_import")tilo::ForgetServiceCookies(d);bad=true;break;}
      if(d->GetString("op")=="resize") TraceGeometry("resize-received",d->HasKey("tab")?d->GetInt("tab"):1,d->GetInt("width"),d->GetInt("height"),Number(d,"scale_factor"));
      CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<CefDictionaryValue> command){Dispatch(command);},d));
      volatile char* consumed=buffer.data();for(size_t i=0;i<size+4;++i)consumed[i]=0;
      const auto previous_size=buffer.size();buffer.erase(0,size+4);
      const auto remaining_size=buffer.size();buffer.resize(previous_size,'\0');buffer.resize(remaining_size);
    }
    if(bad) { std::cerr<<"Malformed browser IPC packet\n";break; }
  }
  tilo::WipeServiceTransport(buffer);
  if(!stop) CefPostTask(TID_UI,base::BindOnce(&CloseHost));
}
}
CefRefPtr<CefApp> MakeHostApp() { return new App; }
void CloseHost() {
  TraceGeometry("close-host",0);
  closing=true;
  if(downloads) downloads->Close();
  if(clients.empty()) {CefQuitMessageLoop();return;}
  const auto live=clients;for(const auto& [id,client]:live) if(client->browser_) {client->CancelMenu();client->browser_->GetHost()->CloseDevTools();client->browser_->GetHost()->CloseBrowser(true);}
}
int RunHost(int argc,char** argv) {
  signal(SIGPIPE,SIG_IGN);
  std::string profile;
  // The frame memory is mapped and its descriptor closed before anything
  // else opens one: Chromium's pipe handler below takes 3 and 4 only when free.
  std::unique_ptr<tilo::FrameMemory> frame_memory;
  for(int i=1;i<argc;++i) { std::string arg=argv[i];if(arg.rfind("--frame-memory-fd=",0)==0 && !frame_memory) {char* end=nullptr;const long fd=std::strtol(arg.c_str()+18,&end,10);if(end && !*end && fd>2 && fd<=INT32_MAX)frame_memory=tilo::FrameMemory::Map(static_cast<int>(fd));if(!frame_memory)std::cerr<<"Frame memory unavailable; frames go down the pipe\n";} }
  for(int i=1;i<argc;++i) { std::string arg=argv[i];if(arg.rfind("--profile=",0)==0) profile=arg.substr(10);if(arg.rfind("--downloads=",0)==0) downloads_directory=arg.substr(12);if(arg=="--private") private_mode=true;if(arg=="--tilo-sealed-documents") sealed_documents=true;if(arg=="--tilo-audio-bridge") audio_bridge=true;if(arg=="--tilo-frame-damage") frame_damage=true;if(arg.rfind("--service-session=",0)==0) {service_session_id=arg.substr(18);if(!tilo::ServiceSessionId(service_session_id))return 2;} }
  if(private_mode && !service_session_id.empty())return 2;
  if(profile.empty() || profile[0]!='/') {std::cerr<<"--profile requires an absolute directory\n";return 2;}
  std::filesystem::create_directories(profile);
  profile=std::filesystem::canonical(profile).string();
  if(downloads_directory.empty()) downloads_directory=profile+"/downloads";
  extension_menus_file=profile+"/Extension Menus.json";
  Output writer(std::move(frame_memory));output=&writer;
#if defined(OS_LINUX) || defined(OS_MAC)
  // Chromium's pipe handler reads commands on fd 3 and replies on fd 4. Use
  // them only when nothing else holds those descriptors. Extensions are
  // loaded, turned off and removed while the browser runs through it, on
  // macOS as on Linux.
  if(fcntl(3,F_GETFD)<0 && fcntl(4,F_GETFD)<0) {
    // Keep every pipe end clear of 3 and 4 until it is moved there on purpose.
    auto high=[](int fd){int moved=fcntl(fd,F_DUPFD_CLOEXEC,5);close(fd);return moved;};
    // macOS has no pipe2; each end is moved high, close-on-exec, at once.
    auto open_pipe=[](int ends[2]){return pipe(ends)==0 && fcntl(ends[0],F_SETFD,FD_CLOEXEC)==0 && fcntl(ends[1],F_SETFD,FD_CLOEXEC)==0;};
    int commands[2],replies[2];
    if(open_pipe(commands) && open_pipe(replies)) {
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
  if(!service_session_id.empty())settings.persist_session_cookies=true;
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
  // Saved passwords live in the login keychain only for an engine with a team
  // identity: an item one ad hoc build saved would make every rebuild ask the
  // person for it again (as tilo-secrets never lets an unsigned build ask).
  if(!private_mode && HostHasStableSigningIdentity()) {
    auto store=tilo::MakeMacCredentialStore(tilo::CredentialServiceForProfile(profile));
    if(store)credential_worker=std::make_shared<tilo::CredentialWorker>(false,std::move(store));
  }
#endif
  {std::lock_guard lock(credential_delivery->mutex);credential_delivery->active=true;}
  std::atomic<bool> stop{false};std::thread reader([&]{ReadCommands(stop);});
  std::thread devtools_reader;if(devtools_read>=0) devtools_reader=std::thread([&]{ReadDevToolsReplies(stop);});
  CefRunMessageLoop();TraceGeometry("message-loop-return",0);stop=true;reader.join();if(devtools_reader.joinable())devtools_reader.join();TraceGeometry("reader-joined",0);
  {std::lock_guard lock(credential_delivery->mutex);credential_delivery->active=false;}
  if(credential_worker)credential_worker->Stop();credential_worker.reset();extension_tabs.reset();pipe_calls.clear();clients.clear();shared_context=nullptr;request_filter=nullptr;TraceGeometry("cef-shutdown-begin",0);CefShutdown();TraceGeometry("cef-shutdown-end",0);writer.Stop();output=nullptr;
  if(clear_on_exit) { std::error_code ec;std::filesystem::remove_all(profile+"/default",ec);if(ec) {std::cerr<<"Could not clear Chromium profile: "<<ec.message()<<"\n";return 1;} }
  return writer.Failed()?1:0;
}
