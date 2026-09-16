#include "host.h"
#include "protocol.h"
#include "key_mapping.h"
#include "request_filter.h"
#include "document_scripts.h"
#include "include/cef_browser.h"
#include "include/cef_client.h"
#include "include/cef_command_line.h"
#include "include/cef_cookie.h"
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
#include <poll.h>
#include <thread>
#include <unistd.h>
#include <signal.h>

namespace {
// CEF callbacks only enqueue: a slow UI must never block Chromium's UI thread.
class Output {
 public:
  Output():thread_([this]{ Run(); }) {}
  ~Output() { Stop(); }
  void Send(uint8_t kind,std::string data) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto packet=qos::EncodePacket(kind,data);
    if(kind==2) frame_=std::move(packet);
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
CefRefPtr<Client> client;
bool closing=false;
CefRefPtr<qos::RequestFilter> request_filter;

class Client final : public CefClient, public CefRenderHandler, public CefLifeSpanHandler,
                     public CefDisplayHandler, public CefLoadHandler, public CefFrameHandler {
 public:
  CefRefPtr<CefRenderHandler> GetRenderHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefFrameHandler> GetFrameHandler() override { return this; }
  void OnFrameCreated(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame) override { SendFilters(frame);SendScripts(frame); }
  void GetViewRect(CefRefPtr<CefBrowser>,CefRect& rect) override { rect=CefRect(0,0,width_,height_); }
  bool GetScreenInfo(CefRefPtr<CefBrowser>,CefScreenInfo& info) override {
    info.device_scale_factor=scale_;info.rect=info.available_rect=CefRect(0,0,width_,height_);return true;
  }
  void OnAfterCreated(CefRefPtr<CefBrowser> browser) override {
    browser_=browser;
    document_scripts_=new qos::DocumentScripts(browser,[this] { FlushNavigation(); },[] { StringEvent("error","message","Document-start script registration failed");CloseHost(); });
    document_scripts_->Attach();
    auto d=CefDictionaryValue::Create();d->SetString("cef_version",CEF_VERSION);d->SetString("chromium_version",std::to_string(CHROME_VERSION_MAJOR)+"."+std::to_string(CHROME_VERSION_MINOR)+"."+std::to_string(CHROME_VERSION_BUILD)+"."+std::to_string(CHROME_VERSION_PATCH));Event("ready",d);
    if(closing) browser_->GetHost()->CloseBrowser(true);
  }
  bool DoClose(CefRefPtr<CefBrowser>) override { return false; }
  void OnBeforeClose(CefRefPtr<CefBrowser> browser) override {
    if(browser_ && browser->IsSame(browser_)) { document_scripts_->Detach();document_scripts_=nullptr;browser_=nullptr;Event("closed");CefQuitMessageLoop(); }
  }
  bool OnBeforePopup(CefRefPtr<CefBrowser> browser,CefRefPtr<CefFrame>,int,const CefString& url,const CefString&,WindowOpenDisposition,bool,const CefPopupFeatures&,CefWindowInfo&,CefRefPtr<CefClient>&,CefBrowserSettings&,CefRefPtr<CefDictionaryValue>&,bool*) override {
    browser->GetMainFrame()->LoadURL(url);return true;
  }
  void OnAddressChange(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,const CefString& url) override { if(frame->IsMain()) StringEvent("uri","url",url); }
  void OnTitleChange(CefRefPtr<CefBrowser>,const CefString& title) override { StringEvent("title","title",title); }
  void OnLoadingProgressChange(CefRefPtr<CefBrowser>,double progress) override { auto d=CefDictionaryValue::Create();d->SetDouble("progress",progress);Event("progress",d); }
  void OnLoadingStateChange(CefRefPtr<CefBrowser>,bool,bool back,bool forward) override { auto d=CefDictionaryValue::Create();d->SetBool("back",back);d->SetBool("forward",forward);Event("history",d); }
  void OnLoadEnd(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,int status) override {
    if(!frame->IsMain()) return; auto d=CefDictionaryValue::Create();d->SetString("url",frame->GetURL());d->SetInt("status",status);Event("loaded",d);SendScripts();
  }
  void OnLoadError(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,ErrorCode code,const CefString& text,const CefString& url) override {
    if(!frame->IsMain() || code==ERR_ABORTED) return; auto d=CefDictionaryValue::Create();d->SetString("url",url);d->SetString("message",text);Event("error",d);
  }
  void OnPopupShow(CefRefPtr<CefBrowser>,bool show) override { popup_visible_=show;if(!show) { popup_.clear(); EmitFrame(); } }
  void OnPopupSize(CefRefPtr<CefBrowser>,const CefRect& rect) override { popup_rect_=rect; }
  void OnPaint(CefRefPtr<CefBrowser>,PaintElementType type,const RectList&,const void* buffer,int width,int height) override {
    auto size=qos::FrameSize(width,height);if(!size) return;
    if(type==PET_VIEW) { pixels_.assign(static_cast<const char*>(buffer),size);pixel_width_=width;pixel_height_=height; }
    else { popup_.assign(static_cast<const char*>(buffer),size);popup_width_=width;popup_height_=height; }
    EmitFrame();
  }
  void EmitFrame() {
    if(pixels_.empty()) return;
    std::string body; qos::AppendU32(body,pixel_width_);qos::AppendU32(body,pixel_height_);body+=pixels_;
    if(popup_visible_ && !popup_.empty()) {
      int x=static_cast<int>(std::round(popup_rect_.x*scale_)),y=static_cast<int>(std::round(popup_rect_.y*scale_));
      for(int row=0;row<popup_height_;++row) {
        int dest_y=y+row;if(dest_y<0 || dest_y>=pixel_height_) continue;
        int start=std::max(0,-x),end=std::min(popup_width_,pixel_width_-x);
        if(end>start) std::memcpy(body.data()+8+(size_t(dest_y)*pixel_width_+x+start)*4,popup_.data()+(size_t(row)*popup_width_+start)*4,size_t(end-start)*4);
      }
    }
    output->Send(2,std::move(body));
  }
  bool OnProcessMessageReceived(CefRefPtr<CefBrowser>,CefRefPtr<CefFrame> frame,CefProcessId source,CefRefPtr<CefProcessMessage> message) override {
    if(source!=PID_RENDERER || message->GetName()!="qos.config") return false;
    SendFilters(frame);SendScripts(frame);return true;
  }
  void SendFilters(CefRefPtr<CefFrame> frame) {
    if(!frame) return;
    auto message=CefProcessMessage::Create("qos.filters");auto args=message->GetArgumentList();
    args->SetString(0,filter_rules_);args->SetBool(1,filter_enabled_);
    frame->SendProcessMessage(PID_RENDERER,message);
  }
  void SendScripts(CefRefPtr<CefFrame> frame=nullptr) {
    if(!frame) { if(!browser_) return;frame=browser_->GetMainFrame(); }
    auto message=CefProcessMessage::Create("qos.scripts");auto args=message->GetArgumentList();
    args->SetString(0,qos::ScriptSource(scripts_));
    frame->SendProcessMessage(PID_RENDERER,message);
  }
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
    if((op=="navigate" || op=="back" || op=="forward" || op=="reload") && document_scripts_->Busy()) {
      if(pending_navigation_.size()>=256) { StringEvent("error","message","Document-start navigation queue exhausted");CloseHost();return; }
      pending_navigation_.push_back(d->Copy(false));return;
    }
    auto host=browser_->GetHost();auto frame=browser_->GetMainFrame();
    if(op=="navigate") frame->LoadURL(d->GetString("url"));
    else if(op=="back") browser_->GoBack();
    else if(op=="forward") browser_->GoForward();
    else if(op=="reload") browser_->Reload();
    else if(op=="stop") browser_->StopLoad();
    else if(op=="focus") host->SetFocus(d->GetBool("focused"));
    else if(op=="resize") {
      int w=d->GetInt("width"),h=d->GetInt("height"); double scale=Number(d,"scale_factor");
      if(!std::isfinite(scale)||scale<0.25||scale>8||!qos::FrameSize(w,h)) { CloseHost(); return; }
      // IPC sizes and pointer coordinates are physical pixels; CEF uses DIP.
      width_=static_cast<int>(std::ceil(w/scale));height_=static_cast<int>(std::ceil(h/scale));scale_=scale;popup_.clear();pixels_.clear();host->NotifyScreenInfoChanged();host->WasResized();
    } else if(op=="mouse_move" || op=="mouse_button" || op=="scroll") {
      CefMouseEvent event;event.x=static_cast<int>(Number(d,"x")/scale_);event.y=static_cast<int>(Number(d,"y")/scale_);event.modifiers=mouse_modifiers_;
      if(op=="mouse_move") host->SendMouseMoveEvent(event,false);
      else if(op=="scroll") host->SendMouseWheelEvent(event,static_cast<int>(Number(d,"dx")/scale_),static_cast<int>(Number(d,"dy")/scale_));
      else { std::string button=d->GetString("button");auto type=button=="right"?MBT_RIGHT:button=="middle"?MBT_MIDDLE:MBT_LEFT;bool down=d->GetBool("down");
        int flag=type==MBT_RIGHT?EVENTFLAG_RIGHT_MOUSE_BUTTON:type==MBT_MIDDLE?EVENTFLAG_MIDDLE_MOUSE_BUTTON:EVENTFLAG_LEFT_MOUSE_BUTTON;
        if(down) mouse_modifiers_|=flag;else mouse_modifiers_&=~flag; event.modifiers=mouse_modifiers_;host->SendMouseClickEvent(event,type,!down,1); }
    } else if(op=="key") Key(d);
    else if(op=="scripts") { scripts_.clear();auto list=d->GetList("scripts");if(list) for(size_t i=0;i<list->GetSize();++i) scripts_.push_back(list->GetString(i));document_scripts_->Configure(qos::ScriptSource(scripts_));SendScripts(); }
    else if(op=="devtools") { CefWindowInfo window;
      host->ShowDevTools(window,new DevToolsClient,CefBrowserSettings(),CefPoint());
    } else if(op=="clear_data") {
      host->GetRequestContext()->GetCookieManager(nullptr)->DeleteCookies("","",nullptr);
      host->GetRequestContext()->ClearHttpCache(nullptr);host->GetRequestContext()->ClearHttpAuthCredentials(nullptr);
    } else if(op=="filter_rules") {
      const auto rules=d->GetString("rules").ToString();
      const bool enabled=d->GetType("enabled")==VTYPE_NULL ? filter_enabled_ : d->GetBool("enabled");
      if(!request_filter->Configure(rules,enabled)) { StringEvent("error","message","Native request filter compilation failed; previous rules retained");return; }
      filter_rules_=rules;filter_enabled_=enabled;
      std::vector<CefString> ids;browser_->GetFrameIdentifiers(ids);
      for(const auto& id:ids) SendFilters(browser_->GetFrameByIdentifier(id));
    }
    else { StringEvent("error","message","Unknown command: "+op);CloseHost(); }
  }
  void Key(CefRefPtr<CefDictionaryValue> d) {
    CefKeyEvent e;e.type=d->GetBool("down")?KEYEVENT_RAWKEYDOWN:KEYEVENT_KEYUP;
    e.modifiers=(d->GetBool("ctrl")?EVENTFLAG_CONTROL_DOWN:0)|(d->GetBool("shift")?EVENTFLAG_SHIFT_DOWN:0)|(d->GetBool("alt")?EVENTFLAG_ALT_DOWN:0)|(d->GetBool("meta")?EVENTFLAG_COMMAND_DOWN:0);
    std::string key=d->GetString("key");
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
  CefRefPtr<qos::DocumentScripts> document_scripts_;std::vector<CefRefPtr<CefDictionaryValue>> pending_navigation_;
  std::string filter_rules_;bool filter_enabled_=true;
  int width_=1280,height_=800,pixel_width_=0,pixel_height_=0,popup_width_=0,popup_height_=0,mouse_modifiers_=0;
  float scale_=1;bool popup_visible_=false;CefRect popup_rect_;std::string pixels_,popup_;std::vector<CefString> scripts_;
  IMPLEMENT_REFCOUNTING(Client);
};
class App final : public CefApp,public CefBrowserProcessHandler {
 public:
  CefRefPtr<CefBrowserProcessHandler> GetBrowserProcessHandler() override { return this; }
  void OnContextInitialized() override {
    client=new Client;CefWindowInfo window;window.SetAsWindowless(0);CefBrowserSettings settings;settings.windowless_frame_rate=30;
    request_filter=new qos::RequestFilter([] { StringEvent("error","message","Native request matcher failed to check a resource"); });
    auto context=CefRequestContext::CreateContext(CefRequestContext::GetGlobalContext(),request_filter);
    if(!CefBrowserHost::CreateBrowser(window,client,"about:blank",settings,nullptr,context)) CefQuitMessageLoop();
  }
 private: IMPLEMENT_REFCOUNTING(App);
};
bool ValidCommand(CefRefPtr<CefDictionaryValue> d) {
  auto op=d->GetString("op").ToString();
  auto type=[&](const char* key,cef_value_type_t expected){return d->GetType(key)==expected;};
  auto number=[&](const char* key){auto t=d->GetType(key);return t==VTYPE_INT || (t==VTYPE_DOUBLE && std::isfinite(d->GetDouble(key)));};
  if(op=="navigate") return type("url",VTYPE_STRING) && !d->GetString("url").empty();
  if(op=="focus") return type("focused",VTYPE_BOOL);
  if(op=="resize") return type("width",VTYPE_INT) && type("height",VTYPE_INT) && number("scale_factor");
  if(op=="mouse_move" || op=="mouse_button" || op=="scroll") {
    if(!number("x") || !number("y") || std::abs(Number(d,"x"))>1000000 || std::abs(Number(d,"y"))>1000000) return false;
    if(op=="scroll") return number("dx") && number("dy") && std::abs(Number(d,"dx"))<=1000000 && std::abs(Number(d,"dy"))<=1000000;
    if(op=="mouse_button") { auto b=d->GetString("button");return type("down",VTYPE_BOOL) && (b=="left" || b=="middle" || b=="right"); }
    return true;
  }
  if(op=="key") return type("key",VTYPE_STRING) && type("down",VTYPE_BOOL);
  if(op=="scripts") {
    if(!type("scripts",VTYPE_LIST)) return false;
    auto list=d->GetList("scripts");for(size_t i=0;i<list->GetSize();++i) if(list->GetType(i)!=VTYPE_STRING) return false;
    return true;
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
      auto d=parsed->GetDictionary();if(!ValidCommand(d)) {bad=true;break;}CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<CefDictionaryValue> command){if(client) client->Command(command);},d));
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
  if(client && client->browser_) { client->browser_->GetHost()->CloseDevTools();client->browser_->GetHost()->CloseBrowser(true); }
}
int RunHost(int argc,char** argv) {
  signal(SIGPIPE,SIG_IGN);
  std::string profile;
  for(int i=1;i<argc;++i) { std::string arg=argv[i];if(arg.rfind("--profile=",0)==0) profile=arg.substr(10); }
  if(profile.empty() || profile[0]!='/') {std::cerr<<"--profile requires an absolute directory\n";return 2;}
  std::filesystem::create_directories(profile);
  profile=std::filesystem::canonical(profile).string();
  Output writer;output=&writer;
  CefSettings settings;settings.windowless_rendering_enabled=true;settings.log_severity=LOGSEVERITY_WARNING;
  CefString(&settings.root_cache_path)=profile;CefString(&settings.cache_path)=profile+"/default";
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
  CefRunMessageLoop();stop=true;reader.join();client=nullptr;request_filter=nullptr;CefShutdown();writer.Stop();output=nullptr;
  return writer.Failed()?1:0;
}
