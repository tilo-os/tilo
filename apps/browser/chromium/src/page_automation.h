#pragma once
#include "page_automation_script.h"
#include "key_mapping.h"
#include "include/cef_browser.h"
#include "include/cef_drag_data.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_dialog_handler.h"
#include "include/cef_parser.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <algorithm>
#include <cmath>
#include <deque>
#include <map>
#include <functional>
#include <chrono>
#include <optional>
#include <set>
#include <string>
#include <utility>
#include <vector>

namespace tilo {
// One frame of the page: the engine's id of it, the number the tab calls it
// by, and the hit-testing offset that turns a point in its document into a
// point in the top one.
struct PageFrame { std::string id,loader,address,number;double x=0,y=0; };
// A document an action ran in: one isolated world of one frame, on the
// connection that frame's process answers on (none named for the page's
// own). The page's own document has no holder; a frame's document is held by
// the element `element` of the document `holder`, and the tab calls that
// frame by the number `handle`.
struct PageDocument { int number=0,context=0;std::string session,frame;int holder=0;std::string element;int handle=0; };
// A point in a frame's own document, hit-tested in the top one.
struct Clicked { double x=0,y=0; };

// Documents are numbered across every tab of this host, so a ref read in one
// tab never resolves in another.
inline int page_documents=0;

// Browser-process page reading and operation for the browser's page actions.
// Script runs in an isolated world of one frame — the main frame, or a frame
// an action names by the number a read gave it or by a ref read in it —
// created through DevTools: it shares that document's DOM, but page script can
// neither see nor call it. A frame of another site runs in a process of its
// own and is asked there, on the connection the page's inspector hands out
// for it; nothing reaches into it from the page around it. Clicks and keys go
// through the same CEF input path as a person's, so the page receives trusted
// events with user activation; a click inside a frame is offset by the
// frame's place in the page.
//
// A page's file chooser is the one native surface an agent cannot drive. A
// click a page action caused therefore holds the chooser's callback here
// instead of showing a panel, and the click's reply says so; `fill_files`
// then answers that same callback with the files the app checked, so the page
// receives the chooser's own trusted input and change events. A person's own
// click still opens the platform panel.
class PageAutomation final : public CefDevToolsMessageObserver {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  // CSS pixels in the main frame's viewport.
  using Click=std::function<void(double,double)>;
  using Key=std::function<void(CefRefPtr<CefDictionaryValue>)>;
  // One step of a pointer gesture, in the same CSS pixels.
  using Pointer=std::function<void(CefRefPtr<CefDictionaryValue>)>;
  // One turn of the page's wheel at a point in the page's CSS pixels.
  using Wheel=std::function<void(double,double,double)>;
  PageAutomation(CefRefPtr<CefBrowser> browser,Emit emit,Click click,Key key,Pointer pointer,Wheel wheel,
      std::function<void()> action_began,std::function<void()> action_ended)
      :browser_(browser),emit_(std::move(emit)),click_(std::move(click)),key_(std::move(key)),pointer_(std::move(pointer)),wheel_(std::move(wheel)),
       action_began_(std::move(action_began)),action_ended_(std::move(action_ended)) {}
  void Attach() {
    registration_=browser_->GetHost()->AddDevToolsMessageObserver(this);
    binding_=true;Tell(std::string(),"Runtime.addBinding",NoteBinding(0));
  }
  void Detach() { registration_=nullptr;browser_=nullptr;pending_.reset();asked_=nullptr;waiting_.clear();CancelFileDialog(); }
  // A page asked for files. A click this automation is driving holds the
  // chooser for `fill_files`; anything else keeps the platform panel.
  bool HoldFileDialog(CefRefPtr<CefFileDialogCallback> callback) {
    if(!pending_ || Action()!="click") return false;
    // One page, one chooser: a newer request supersedes an unanswered one.
    CancelFileDialog();
    file_dialog_=callback;file_dialog_request_=pending_->request;
    // A page whose chooser is never answered holds the browser's own file
    // work open forever, so the hold ends on its own as well.
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) {
      if(self->browser_) self->CancelFileDialog();
    },CefRefPtr<PageAutomation>(this)),kFileDialogWaitMs);
    return true;
  }
  // A main-frame navigation began. An action that caused one answers once it
  // loads, so the caller's next read sees the page the action opened. The
  // document a held file chooser belonged to is gone, so that chooser is
  // cancelled rather than answered for a page nobody is looking at.
  void NavigationStarted() {
    ++navigations_;CancelFileDialog();
    if(!pending_) return;
    // The document the action ran in is being replaced: a wait for the page's
    // fence, or for a frame's settle, can no longer be answered from it.
    // Waiting for the navigation to load is what answers this action now.
    pending_->awaiting_start=false;
    pending_->waits_on_document=false;
    if(!pending_->awaiting_load) pending_->awaiting_load=true;
  }
  void NavigationFinished() {
    if(!pending_ || !pending_->awaiting_load) return;
    // The load an action waits for can end before the page's fence has said
    // what the action set off (a form posted to a server that answers at
    // once). The page began this navigation while it handled the action,
    // which is what the answer then says: it is not one still to come.
    if(pending_->navigation=="possible" && navigations_!=pending_->navigations) pending_->navigation="started";
    Succeed(pending_->message,std::nullopt);
  }
  // The page's own frame committed a new document. Every script context of
  // the document before it is gone, and the renderer the new one runs in may
  // be another, which numbers its contexts from the start again: a context
  // kept from the old document would then name some other context of the
  // new page, a frame's or a world that is not this one's. Nothing read from
  // the old page is kept.
  void DocumentCommitted() { ForgetPage(); }
  // The page's renderer is gone (it crashed, or was replaced): no answer can
  // come from a document that no longer exists. The action in flight is
  // answered as an outcome nobody knows, so it never stays accepted.
  void RendererGone() {
    if(!pending_) return;
    Finish(Failure("page_gone","The page's renderer went away while this action ran, so whether "
      "it took effect is unknown; read the page again before acting on it"));
  }
  // The tab asked this action to stop. The page cannot be told mid-request, so
  // the answer is the one thing that is true: it was not observed to finish,
  // and whether it took effect is unknown.
  //
  // Several actors may have actions in one tab, one running and others
  // waiting their turn, so a caller that gives up names the request it gives
  // up on: one still waiting is taken out before it ran, and is known to
  // have changed nothing; the one running is stopped as above; any other is
  // left alone. A cancel that names no request stops the one running.
  void Cancel(int request=0) {
    if(request>0) {
      for(auto it=waiting_.begin();it!=waiting_.end();++it) {
        if((*it)->GetInt("request_id")!=request) continue;
        waiting_.erase(it);
        Unrun(request,Failure("cancelled","The action was cancelled while it waited its turn in this tab, so it never ran and changed nothing"));
        return;
      }
      if(!pending_ || pending_->request!=request) return;
    }
    if(!pending_) return;
    Finish(Failure("cancelled","The action was cancelled before the page answered it, so whether "
      "it took effect is unknown; read the page again to see what it did"));
  }
  // A window an agent operates held something its page was about to send
  // (departures.h): a form submission, a request with data, a download. The
  // tab's record says so, as it says what a person typed: no action made the
  // entry, so it has `how: "hold"`, the kind of what waits as its `action`,
  // where it would go as `to_url` and its method there (of a download the
  // file it would save) as its message. It is the actor's whose action was
  // running when the page reached it, and nobody's when the page sent it on
  // its own.
  void HoldBegan(const std::string& kind,const std::string& method,const std::string& origin,const std::string& file) {
    NoteHold(kind,pending_ && pending_->seq?pending_->actor:std::string(),method,origin,file);
  }
  // A held departure ended, and the record says how: it was let leave
  // (`allow`), the page was kept as it is (`keep`), it was dropped with
  // nobody answering (`drop`), it was turned away as one too many
  // (`too_many`), or the page it was held on went away (`gone`). `actor` is
  // who answered, as the app says: nobody's name for the person at the
  // window. `page` is the page it was held on, when the tab shows another by
  // now.
  void HoldEnded(const std::string& how,const std::string& actor,const std::string& method,const std::string& origin,const std::string& file,const std::string& page={}) {
    NoteHold(how,actor,method,origin,file,page);
  }
  // What was done in this tab, in the order it was done, after the place
  // `since` names: who asked, what they asked for, what it acted on and how
  // it ended. It reads nothing of the page, so it answers at once whatever
  // is running, and says what is running and how many wait behind it.
  void Ledger(CefRefPtr<CefDictionaryValue> d) {
    const double since=d->GetType("since")==VTYPE_DOUBLE?d->GetDouble("since"):d->GetInt("since");
    auto list=CefListValue::Create();size_t count=0;bool more=false;double last=since;
    for(const auto& entry:ledger_) {
      if(entry->GetDouble("seq")<=since) continue;
      if(count>=kMaxLedgerRead) { more=true;break; }
      list->SetDictionary(count++,entry->Copy(false));last=entry->GetDouble("seq");
    }
    auto data=CefDictionaryValue::Create();
    data->SetList("entries",list);data->SetDouble("next",last);data->SetBool("more",more);
    data->SetDouble("dropped",static_cast<double>(ledger_dropped_));
    data->SetInt("waiting",static_cast<int>(waiting_.size()));
    if(pending_ && pending_->seq) {
      auto running=CefDictionaryValue::Create();
      running->SetDouble("seq",static_cast<double>(pending_->seq));
      running->SetString("actor",pending_->actor);running->SetString("action",pending_->asked);
      data->SetDictionary("running",running);
    }
    if(d->GetType("actor")==VTYPE_STRING) Seen(d->GetString("actor").ToString(),static_cast<uint64_t>(last));
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",true);
    fields->SetString("url",browser_?browser_->GetMainFrame()->GetURL():CefString());
    fields->SetString("message",std::to_string(count)+(count==1?" action":" actions")+(more?", and more after them":""));
    fields->SetString("navigation","none");fields->SetBool("navigated",false);fields->SetBool("loading",false);fields->SetNull("outline");
    fields->SetDictionary("data",data);fields->SetInt("request_id",d->GetInt("request_id"));
    emit_("page_result",fields);
  }
  // Whether an action is running: the tab refuses another one while it is.
  bool Busy() const { return static_cast<bool>(pending_); }
  // A request the engine refused before it reached the page is answered in
  // the order the caller asked, behind anything still running. The tab is
  // told the request is settled here, so the same request reaching it after
  // this answer runs nothing.
  void Refuse(CefRefPtr<CefDictionaryValue> command,const std::string& code,const std::string& message) {
    const int request=command->GetInt("request_id");
    if(!browser_ || refused_.count(request)) return;
    // The tab's copy of each refused request is on its way here, so the set
    // holds what is still to arrive. A caller that sends refusals faster than
    // the tab answers them makes room from the oldest, which by then arrived.
    if(refused_.size()>=kMaxRefused) refused_.erase(refused_.begin());
    refused_.insert(request);
    Reply(request,Failure(code,message),command->GetString("op")=="text_input");
    RunQueued();
  }
  // The requests that waited on a refusal run now, in the order they came:
  // each waits for the one before it, and the queue drains as they answer.
  void RunQueued() {
    if(queued_.empty()) return;
    auto next=queued_.front();queued_.erase(queued_.begin());
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,CefRefPtr<CefDictionaryValue> command) {
      self->Run(command);
    },CefRefPtr<PageAutomation>(this),next));
  }
  // A request that came while a refusal was still answering waits behind it,
  // in the order the caller asked, however long that answer takes.
  void Request(CefRefPtr<CefDictionaryValue> command) {
    if(!queued_.empty()) { queued_.push_back(command->Copy(false));return; }
    Run(command);
  }
  // `turn` says the request is the one whose turn has come: the first of
  // those that waited, which runs ahead of whatever arrived since.
  void Run(CefRefPtr<CefDictionaryValue> command,bool turn=false) {
    const int request=command->GetInt("request_id");
    if(!browser_) return;
    // Several actors may work in one tab, so an action that arrives while
    // another runs waits its turn and runs in the order it came, rather than
    // being refused and left to its caller to ask again. One that arrives in
    // the moment the tab is free while others still wait goes behind them,
    // never ahead. A private text edit never waits: its lease is of the
    // moment it was asked in.
    if((pending_ || (!turn && !waiting_.empty())) && !refused_.count(request) && command->GetString("op")!="text_input") {
      if(waiting_.size()>=kMaxWaiting) Unrun(request,Failure("busy",Holding()));
      else {
        waiting_.push_back(command->Copy(false));
        CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,int request) { self->WaitedOut(request); },
          CefRefPtr<PageAutomation>(this),request),kMaxWaitMs);
      }
      if(!pending_) CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->RunWaiting(); },CefRefPtr<PageAutomation>(this)));
      RunQueued();return;
    }
    // An action runs over the page as if it were being looked at: a page the
    // window is not showing never settles, never loads and takes no wheel.
    if(action_began_) action_began_();
    shown_=true;
    // A request the engine answered for this tab already is not run again:
    // it was judged before it ever reached the page.
    if(refused_.erase(request)) { RunQueued();return; }
    if(pending_) {
      // The request that is running answers next, and the ones that waited on
      // a refusal behind this one run as it does.
      Reply(request,Failure("busy","Another page action is still running in this tab"),command->GetString("op")=="text_input");
      RunQueued();return;
    }
    const bool private_text=command->GetString("op")=="text_input";
    auto owned=command->Copy(false);
    if(private_text) {
      const double now=std::chrono::duration<double,std::milli>(std::chrono::system_clock::now().time_since_epoch()).count();
      if(!owned->HasKey("expires_ms")) owned->SetDouble("expires_ms",now+5000);
      if(Number(owned,"expires_ms")<=now || Number(owned,"expires_ms")>now+10000) {
        Reply(request,Failure("text_input_expired","The original text request expired"),true);return;
      }
    }
    native_attempted_=false;native_object_.clear();
    capture_=Area{};capture_label_.clear();
    // The page's inspector was away and is back: it is told again what the
    // page's own world may call.
    if(!binding_) { binding_=true;Tell(std::string(),"Runtime.addBinding",NoteBinding(0)); }
    pending_=Pending(request,++token_,navigations_,false,owned,std::string{});
    pending_->asked=owned->GetString("action").ToString();
    pending_->actor=owned->GetType("actor")==VTYPE_STRING?owned->GetString("actor").ToString():std::string();
    pending_->began_url=browser_->GetMainFrame()->GetURL().ToString();
    pending_->asked_command=owned;
    if(!private_text && Operates(pending_->asked)) pending_->seq=++acted_seq_;
    // A call still in flight belongs to the request it was sent for: the new
    // request's answers are matched against calls of its own only.
    call_=0;
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) { self->Fail("timeout","The page did not answer in time"); }
    },CefRefPtr<PageAutomation>(this),token_),private_text?5000:kTimeoutMs);
    // An action starts in the page's own document, whatever frame the one
    // before it ran in.
    asked_=nullptr;LeaveFrame();
    Begin();
  }
  // The page started a drag from the press the pointer just made. Offscreen
  // CEF offers no drag loop, so this host is the drag's target too: it drags
  // over the points the gesture asks for and drops at its end, which the
  // gesture's own step loop drives. A page drag nothing is driving is refused,
  // which is how such a drag ends.
  // Whether this automation's own gesture is the thing dragging the page.
  bool DrivesDrag() const { return pending_ && Action()=="drag"; }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    // The body belongs to this callback alone, so it is copied now; the answer
    // is applied on this same turn, where a call made from it is the answer's
    // own call: a turn of its own would let the next call be sent first, and
    // the two answers would arrive under each other's stage.
    ApplyAnswer(id,success,std::string(size>0?static_cast<const char*>(result):"",size>0?size:0));
  }
  // A frame of another site answers on a connection of its own, and what it
  // says there names that connection, which nothing said on the page's own
  // does: the two are told apart here, before the page's answers are read.
  bool OnDevToolsMessage(CefRefPtr<CefBrowser>,const void* message,size_t size) override {
    if(!size) return false;
    const std::string json(static_cast<const char*>(message),size);
    if(json.find("\"sessionId\"")==std::string::npos) return false;
    auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
    auto body=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    if(!body || body->GetType("sessionId")!=VTYPE_STRING) return false;
    if(body->GetType("method")==VTYPE_STRING) {
      // A frame of another site may hold frames of yet another: its own
      // connection says when one of those is handed out or taken back.
      const auto method=body->GetString("method").ToString();
      if(method=="Target.attachedToTarget") TargetAttached(body->GetDictionary("params"));
      else if(method=="Target.detachedFromTarget") TargetDetached(body->GetDictionary("params"));
      // What a frame's own document tells without being asked (see `Noted`).
      else if(method=="Runtime.bindingCalled") Noted(body->GetString("sessionId").ToString(),body->GetDictionary("params"));
      return true;
    }
    if(body->GetType("id")!=VTYPE_INT) return true;
    auto said=body->GetDictionary("result");
    const bool success=static_cast<bool>(said);
    if(!said) said=body->GetDictionary("error");
    auto value=CefValue::Create();
    value->SetDictionary(said?said->Copy(false):CefDictionaryValue::Create());
    ApplyAnswer(body->GetInt("id"),success,CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString());
    return true;
  }
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override {
    loader_.clear();world_loader_.clear();text_owner_.reset();text_lease_.clear();
    ForgetPage();target_sessions_.clear();session_targets_.clear();auto_attach_=false;binding_=false;
    ++generation_;
    if(pending_ && call_) Fail("unavailable","The page's inspector connection closed");
  }

 private:
  // The answer of a call is applied on a turn of its own, and the call it
  // chains is sent from there: a call is only ever sent after the previous
  // answer has been read, so two answers never arrive under each other's stage.
  void ApplyAnswer(int id,bool success,const std::string& json) {
    if(!browser_ || !pending_ || !call_ || id!=call_) return;
    // The answer is its call's own turn: the next call is sent only after this
    // answer has read it, so a second answer is never taken for it.
    call_=0;stage_=stage_at_call_;
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,bool success,std::string json) {
      if(self->pending_) self->AdvanceStage(success,json);
    },CefRefPtr<PageAutomation>(this),success,json));
  }
  static constexpr int64_t kTimeoutMs=30000;
  // How long a frame between two processes, or one whose own connection has
  // not been handed out yet, is waited for, and how often it is asked again
  // meanwhile.
  static constexpr double kFrameWaitMs=1500;
  static constexpr int64_t kFrameWaitStepMs=50;
  // The frames of a page that are read with it: as many as that, each by
  // its first lines. The rest of a frame is read with the frame's number.
  static constexpr size_t kShownFrames=6;
  static constexpr int kShownLines=40;
  // The frames whose fields a review of the page reads with the page's own.
  static constexpr size_t kReviewFrames=8;
  // The numbers frames are called by, and the connections kept for frames of
  // other sites.
  static constexpr int kMaxFrameHandle=999;
  static constexpr size_t kMaxFrameSessions=256;
  // Calls made on a frame's own connection are numbered here, far above the
  // numbers the engine gives the calls it numbers itself.
  static constexpr int kFirstSessionCall=1000000000,kLastSessionCall=2000000000;
  // What stands either side of the ref the page's script names a frame by.
  static constexpr char kFrameMark='\x01';
  // A hover's styles have run by the next frame; a release's handlers by
  // shortly after the frame that carried it.
  static constexpr int64_t kHoverFrameMs=80;
  static constexpr int64_t kReleaseFrameMs=80;
  // A drag's first move waits a frame for the drag the press began.
  static constexpr int64_t kPathPointMs=16;
  // A drawn path's next point is sent when the page has heard the one
  // before (see `PathHeard`): how long that is waited for after the press,
  // after a move, and how much longer for a page that never answers.
  static constexpr int64_t kPathPressMs=80,kPathHeardMs=1000,kPathSlackMs=150;
  // How many points in a row a page may not hear before the rest are sent
  // by the clock.
  static constexpr int kPathMissed=2;
  // One turn of the scroll wheel is one frame: a page's scroll listeners and
  // intersection observers run between two turns, as they do under a hand,
  // so a lazy list loads while the wheel keeps turning.
  static constexpr int64_t kScrollWheelMs=40;
  // The most one scroll may turn the wheel, and the most one turn travels:
  // bounded work whatever the page says is left, and an answer inside the
  // time an action is waited for.
  static constexpr int kMaxScrollTurns=64;
  // The most wheel events one turn goes out as.
  static constexpr int kMaxWheelPieces=64;
  // The refusals still to be met by the tab's copy of the request.
  static constexpr size_t kMaxRefused=256;
  // A scroll answers inside the time an action is waited for, however much
  // page a lazy list keeps giving: what the wheel reached by then is its
  // answer, and a caller that wants more scrolls again. A wheel moves a box
  // one visible page of it a turn and each turn is waited for until the box
  // has shown it, so a distance of many pages takes a while on a busy
  // machine: the turns it may take are what bounds the work, and this is
  // the time they are given, well inside the wait for an action's answer.
  static constexpr double kScrollBudgetMs=15000;
  static constexpr double kMaxScrollTurn=2000;
  // A started navigation is awaited this long; a slower one is reported as
  // still loading and the caller waits for it.
  static constexpr int64_t kLoadWaitMs=10000;
  // A held file chooser waits this long for `fill_files`, then is cancelled so
  // the page is never left waiting on input that will not come.
  static constexpr int64_t kFileDialogWaitMs=60000;
  enum class Stage { FrameTree, Asked, World, TextWorld, TextFocus, TextFailureFrameTree, NativeElement, NativeDOM, NativeDocument, NativeNode, NativeEditors, NativeResolve, NativeConfigure, FileTarget, FileSet, TypeKey, FillTyped, Evaluate, Paint, Fence, GestureBegin, Path, Hover, Pointer, GestureEnd, FrameSettle, Scroll };
  // A gesture's own stages are numbered above every page stage, so one kind of
  // stage never stands for the other.
  enum class GestureStage { Tree, World, Locate };
  static double Now() {
    return std::chrono::duration<double,std::milli>(std::chrono::system_clock::now().time_since_epoch()).count();
  }
  static constexpr int kGestureBase=1000;
  static constexpr int Gesture(GestureStage stage) { return kGestureBase+static_cast<int>(stage)+1; }
  // Long enough for hover, the press and the drop to paint what they set.
  static constexpr int64_t kPointerFrameMs=60;
  // The most lines one fill into a region types, so one action stays bounded.
  static constexpr size_t kMaxFillRuns=2000;
  // Long enough for one key of a typed fill to reach the page and be handled
  // before the next is sent, as a person's keys arrive one at a time.
  static constexpr int64_t kTypeKeyMs=8;
  // Long enough for the edit that emptied the region to settle before the first
  // key is sent.
  static constexpr int64_t kTypeStartMs=120;
  struct TextFrame { std::string id,loader;int world=0,document=0;bool same_origin=true; };
  struct Pending {
    // Named, so an aggregate initializer names each field once: a member added
    // to the middle of the list cannot silently shift the ones after it.
    int request=0;uint64_t token=0;uint64_t navigations=0;bool retried=false;bool refreshed=false;
    // The frame the action ran in, by the number the tab calls it, and the
    // address of that frame's own document: the tab's record keeps both.
    int frame=0;std::string frame_url;
    // A read of the page's own document, kept while the frames it shows are
    // read into it: what it said, its lines, and each such frame by the line
    // that lists it.
    struct Shown { std::string ref;int at=0;std::vector<std::string> lines;bool more=false; };
    std::string read_message,read_outline;std::vector<Shown> shown;size_t shown_index=0;
    // A review of the page's own fields, kept while the fields of the frames
    // this tab has worked in are read into it.
    CefRefPtr<CefDictionaryValue> review;std::vector<int> review_frames;size_t review_index=0;
    CefRefPtr<CefDictionaryValue> command;std::string message="";
    // "none": the action ran no page code. "started": the page began leaving
    // the document while it handled the action's input. "possible": it had
    // not, and may still navigate later on its own. "frame": what it did, it
    // did inside a frame, which is not a way for the page to leave.
    std::string navigation="none";bool awaiting_start=false,awaiting_load=false;
    // Whether this action's answer is read from the document it acted in. A
    // navigation that begins then takes that document away unanswered.
    bool waits_on_document=false;
    // Where a gesture's two ends are, in the view's space, once it began.
    int from_x=0,from_y=0,to_x=0,to_y=0;int steps=0,step=0;
    // What the page resolved when a drag began, so a drop nothing took can be
    // replayed through the page's own drag and drop handlers.
    CefRefPtr<CefDictionaryValue> drag_fallback;
    // Whether the page's own CSS reveals something on hover, so the hover is
    // mirrored where the pointer rests.
    bool hover_styles=false;
    // A drawing path's points in the view's space, and the one being sent.
    std::vector<std::pair<double,double>> points;size_t point_index=0;bool drawing=false;
    // Who asked, what they asked for and where the page was when they did.
    // The command an action carries is replaced as it is carried out (a
    // gesture ends as a read), and the tab's record is of what was asked.
    std::string actor,asked,began_url;CefRefPtr<CefDictionaryValue> asked_command,target;
    // What the page said of an action that answers only once the page has
    // handled it (a form's answers, field by field), kept for that answer.
    CefRefPtr<CefDictionaryValue> result_data;
    // The place this action takes in the tab's record when it operates the
    // page; a read takes none.
    uint64_t seq=0;
    // Whether what the action wrote went into a field that is secret by its
    // nature, as the page said of the element it acted on, and whether a key
    // went where the page's focus could not be read: neither is set down
    // with what was written.
    bool secret=false,unseen=false;
    // A key pressed without an element: what the page said of where its
    // focus is, kept while the frames that focus is inside are asked.
    bool armed=false;CefRefPtr<CefDictionaryValue> arm;
    // A find or a search that looks on in the frames the page shows: what
    // the page's own document said (and whether its matches are counted),
    // the frames to ask, by their numbers, and what each said.
    CefRefPtr<CefDictionaryValue> look;bool look_counted=false;std::vector<int> look_frames;size_t look_index=0;
    std::vector<std::pair<int,CefRefPtr<CefDictionaryValue>>> looked;
    // A drawn path: the moves of the pointer the page had heard when the
    // last point was sent, which of its points was last sent and which was
    // last gone on from.
    double moves=0;int asked_point=0,heard_point=0;
    // Whether the page heard the pointer arrive at the last point sent, how
    // many points in a row it did not, and whether it is asked no more.
    bool heard_last=false,path_blind=false;int path_missed=0;
    // Whether a gesture of pointer steps has told the page that it began.
    bool acting_told=false;
    Pending()=default;
    Pending(int request,uint64_t token,uint64_t navigations,bool retried,CefRefPtr<CefDictionaryValue> command,
      std::string message)
      :request(request),token(token),navigations(navigations),retried(retried),command(std::move(command)),
       message(std::move(message)) {}
  };
  // Gestures an agent asks for, which start from the page's own layout.
  bool Dragging() const {
    const auto action=Action();
    return action=="hover" || action=="drag" || action=="path";
  }
  // A hover at a viewport point travels as `at` with `hover` set, so the point
  // reads like every other hover: the page resolves it, the pointer rests
  // there, and the reply says what the hover revealed.
  bool HoverAtPoint() const { return Action()=="at" && pending_->command->GetBool("hover"); }
  bool Gesturing() const { return Dragging() || HoverAtPoint(); }
  std::string Action() const { return pending_->command->GetString("action").ToString(); }
  bool PrivateText() const { return pending_->command->GetString("op")=="text_input"; }
  // The number a caller names a frame by: what the outline printed for it, a
  // whole number from 1. Nothing else names a frame, so a word, a selector or
  // a path of numbers is refused, never read as the page.
  static int FrameHandle(const std::string& named) {
    if(named.empty() || named.size()>3 || named.front()=='0') return 0;
    int value=0;
    for(const char c:named) { if(c<'0'||c>'9') return 0;value=value*10+(c-'0'); }
    return value;
  }
  // The document an element's ref was read in: the number between its `d`
  // and its `e`, or none for what is not a ref.
  static int RefDocument(const std::string& ref) {
    const auto e=ref.find('e');
    if(ref.empty() || ref.size()>32 || ref.front()!='d' || e==std::string::npos || e<2 || e>11 || e+1>=ref.size()) return 0;
    long long value=0;
    for(size_t i=1;i<e;++i) { if(ref[i]<'0'||ref[i]>'9') return 0;value=value*10+(ref[i]-'0'); }
    for(size_t i=e+1;i<ref.size();++i) if(ref[i]<'0'||ref[i]>'9') return 0;
    return value>0 && value<=2147483647?static_cast<int>(value):0;
  }
  // The number the tab calls the frame an element holds. A frame is given
  // its number when a read first lists it and keeps it for as long as the tab
  // lives: the frames that come and go around it take other numbers, never
  // its own, and the number of a frame that has left names nothing. Only
  // when every number has been given is one whose page is gone given again.
  int HandleOf(const std::string& ref) {
    if(const auto known=frame_handles_.find(ref);known!=frame_handles_.end()) return known->second;
    int handle=0;
    for(int tried=0;tried<kMaxFrameHandle && !handle;++tried) {
      last_handle_=last_handle_%kMaxFrameHandle+1;
      const auto held=frame_refs_.find(last_handle_);
      if(held==frame_refs_.end() || !docs_.count(RefDocument(held->second))) handle=last_handle_;
    }
    if(!handle) handle=last_handle_=last_handle_%kMaxFrameHandle+1;
    if(const auto old=frame_refs_.find(handle);old!=frame_refs_.end()) {
      frame_handles_.erase(old->second);frame_labels_.erase(handle);
      std::vector<int> held;
      for(const auto& [number,document]:docs_) if(document.handle==handle) held.push_back(number);
      for(const int number:held) Forget(number);
    }
    frame_refs_[handle]=ref;frame_handles_[ref]=handle;
    return handle;
  }
  // What the page's script wrote of a frame is the ref of the element that
  // holds it, between two marks: the number the tab calls that frame by is
  // written in its place. Only the ref of a document this tab is reading
  // names a frame; marks a page wrote into its own text name none.
  std::string Numbered(const std::string& text) {
    if(text.find(kFrameMark)==std::string::npos) return text;
    std::string out;out.reserve(text.size());
    size_t at=0;
    while(at<text.size()) {
      const auto open=text.find(kFrameMark,at);
      if(open==std::string::npos) { out.append(text,at,std::string::npos);break; }
      out.append(text,at,open-at);
      const auto close=text.find(kFrameMark,open+1);
      const auto ref=close==std::string::npos?std::string():text.substr(open+1,close-open-1);
      if(close==std::string::npos || !docs_.count(RefDocument(ref))) { at=open+1;continue; }
      out+=std::to_string(HandleOf(ref));at=close+1;
    }
    return out;
  }
  void NumberIn(CefRefPtr<CefDictionaryValue> value,int depth=0) {
    if(!value || depth>8) return;
    CefDictionaryValue::KeyList keys;value->GetKeys(keys);
    for(const auto& key:keys) {
      const auto type=value->GetType(key);
      if(type==VTYPE_STRING) {
        const auto text=value->GetString(key).ToString();
        if(text.find(kFrameMark)!=std::string::npos) value->SetString(key,Numbered(text));
      } else if(type==VTYPE_DICTIONARY) NumberIn(value->GetDictionary(key),depth+1);
      else if(type==VTYPE_LIST) NumberIn(value->GetList(key),depth+1);
    }
  }
  void NumberIn(CefRefPtr<CefListValue> list,int depth) {
    if(!list || depth>8) return;
    for(size_t i=0;i<list->GetSize();++i) {
      const auto type=list->GetType(i);
      if(type==VTYPE_STRING) {
        const auto text=list->GetString(i).ToString();
        if(text.find(kFrameMark)!=std::string::npos) list->SetString(i,Numbered(text));
      } else if(type==VTYPE_DICTIONARY) NumberIn(list->GetDictionary(i),depth+1);
      else if(type==VTYPE_LIST) NumberIn(list->GetList(i),depth+1);
    }
  }
  // The frames this tab has listed whose pages are still here, for a caller
  // that named one the tab does not have.
  std::string FramesHere() const {
    std::string listed;int count=0;
    for(const auto& [handle,ref]:frame_refs_) {
      if(!docs_.count(RefDocument(ref))) continue;
      if(++count>12) { listed+=" and more";break; }
      if(!listed.empty()) listed+=", ";
      listed+="[frame="+std::to_string(handle)+"]";
      if(const auto label=frame_labels_.find(handle);label!=frame_labels_.end() && !label->second.empty()) listed+=" \""+label->second+"\"";
    }
    return listed.empty()?"no read of this page has listed a frame":"the frames read on this page are "+listed;
  }
  // A frame named that this tab never listed: the answer says how a frame is
  // named and which frames the page has, so the id that works is one of them.
  std::string NoSuchFrame(const std::string& named) const {
    const std::string asked=named.empty()?"that id":"the id \""+Clip(named,32)+"\"";
    return "No frame of this page has "+asked+"; "+FramesHere()+
      ". A frame is named by the number the outline prints for it, as in [frame=1]";
  }
  // A frame this tab listed that is no longer there: its number is not given
  // to another frame, so the answer can say that it left.
  std::string FrameGone(int handle) const {
    const auto label=frame_labels_.find(handle);
    return "Frame "+std::to_string(handle)+(label!=frame_labels_.end() && !label->second.empty()?" \""+label->second+"\"":std::string())+
      " has left the page, and its number names nothing now; read the page again for the frames it has";
  }
  bool CollectTextFrames(CefRefPtr<CefDictionaryValue> tree,std::vector<TextFrame>& frames,const std::string& root_origin={},bool observed_ancestors=true) {
    if(!tree || frames.size()>=128)return false;
    auto frame=tree->GetDictionary("frame");
    if(!frame || frame->GetType("id")!=VTYPE_STRING || frame->GetType("loaderId")!=VTYPE_STRING || frame->GetType("securityOrigin")!=VTYPE_STRING)return false;
    const auto origin=frame->GetString("securityOrigin").ToString();
    const auto root=root_origin.empty()?origin:root_origin;
    const bool observed=observed_ancestors && origin==root;
    frames.push_back({frame->GetString("id").ToString(),frame->GetString("loaderId").ToString(),0,0,observed});
    auto children=tree->GetList("childFrames");
    if(children)for(size_t i=0;i<children->GetSize();++i)
      if(!CollectTextFrames(children->GetDictionary(i),frames,root,observed))return false;
    return true;
  }
  void ProbeTextFrame() {
    // Cross-origin ancestors cannot be observed without widening DOM authority.
    while(!text_candidates_.empty() && !text_candidates_.front().same_origin)text_candidates_.erase(text_candidates_.begin());
    if(text_candidates_.empty()) { Fail("unavailable","The focused frame has no isolated text context");return; }
    text_frame_=text_candidates_.front();text_candidates_.erase(text_candidates_.begin());
    auto params=CefDictionaryValue::Create();
    params->SetString("frameId",text_frame_.id);params->SetString("worldName","tilo-private-text-input");
    // No universal access, origin bypass or debugger pause is requested.
    Call(Stage::TextWorld,"Page.createIsolatedWorld",params);
  }
  void Begin() {
    // A turn of the wheel starts from what the page shows now, like a gesture:
    // the page's own frame tree says which document the wheel turns in, and
    // the page then says where to turn it.
    if(pending_ && Action()=="scroll") { Call(Stage::FrameTree,"Page.getFrameTree",nullptr);return; }
    // A press with no element is read like any other action first: the
    // page says where its focus is, so the key's edit is known as this
    // action's and a key written into a secret is not named.
    // A chooser this page is waiting on is answered with the page's own
    // completion path, so its input and change events are the picker's, and
    // no DOM node has to be reached while that dialog is open.
    if(Action()=="fill_files" && file_dialog_) { AnswerFileDialog(pending_->command->GetList("paths"));return; }
    // Choosing files resolves the ref exactly as reading the page did, in the
    // document the outline named.
    // A gesture is never a read of a tree an earlier action left: it asks the
    // page for what only the page can say, whatever it already knows.
    // A pointer gesture is placed step by step, in the tree it reads itself.
    if(Action()=="pointer") { BeginGesture();return; }
    // A gesture the page runs itself starts from what the page shows now.
    if(Dragging() || HoverAtPoint()) { GestureDocument();return; }
    // A tree kept from an earlier action still speaks for this one unless the
    // page's frames moved since it was read. A retry is not a first read: the
    // tree it read is the one it reads again.
    if(tree_ && (pending_->refreshed || tree_generation_==generation_)) { AfterTree();return; }
    ReadTree();
  }
  // Answer the held chooser with these paths. The page then runs exactly what
  // a person's pick runs.
  void AnswerFileDialog(CefRefPtr<CefListValue> paths) {
    auto chosen=CefRefPtr<CefFileDialogCallback>(file_dialog_);
    file_dialog_=nullptr;file_dialog_request_=0;
    std::vector<CefString> files;
    if(paths) for(size_t i=0;i<paths->GetSize();++i) files.push_back(paths->GetString(i));
    chosen->Continue(files);
    auto data=CefDictionaryValue::Create();data->SetList("files",FileNames());data->SetString("mode","chooser");
    Succeed(pending_->message,std::nullopt,data);
  }
  // The names of the files this action chose, as the page shows them: a path's
  // own last component, never the folder it came from.
  CefRefPtr<CefListValue> FileNames() {
    auto names=CefListValue::Create();
    auto paths=pending_?pending_->command->GetList("paths"):nullptr;
    for(size_t i=0;paths && i<paths->GetSize();++i) {
      const auto path=paths->GetString(i).ToString();
      const auto slash=path.find_last_of("/\\");
      names->SetString(i,slash==std::string::npos?path:path.substr(slash+1));
    }
    return names;
  }
  // A chooser the page opened and nothing answered is cancelled, so the page
  // is not left waiting on input that will never come.
  void CancelFileDialog() {
    if(!file_dialog_) return;
    auto held=CefRefPtr<CefFileDialogCallback>(file_dialog_);file_dialog_=nullptr;file_dialog_request_=0;
    held->Cancel();
  }
  // The isolated world resolved this step's element and accepted the paths, so
  // what remains is the DOM agent's own work: the files are not writable from
  // script. Resolve the element object again by ref, turn it into a node, and
  // let the input dispatch the picker's events.
  void FileTarget(CefRefPtr<CefDictionaryValue> value) {
    pending_->message=Text(value,"message");
    auto params=CefDictionaryValue::Create();
    params->SetString("expression","globalThis.__tiloPage.fillFilesElement("+CefWriteJSON(pending_->command->GetValue("element"),JSON_WRITER_DEFAULT).ToString()+")");
    params->SetString("objectGroup","tilo-page-file-input");
    params->SetInt("contextId",world_);params->SetBool("silent",true);
    CallIn(Stage::FileTarget,world_session_,"Runtime.evaluate",params);
  }
  // A gesture's steps are resolved one at a time: a step that names an
  // element is placed in that element's box in this document, every other
  // step is already a viewport position.
  void BeginGesture() {
    pointer_index_=0;gesture_=true;SetGesture(0,0);gesture_boxes_.clear();step_located_=false;
    auto steps=pending_->command->GetType("steps")==VTYPE_LIST?pending_->command->GetList("steps"):nullptr;
    if(!steps || steps->GetSize()==0) { Fail("unavailable","A gesture has no steps");return; }
    AdvancePointer();
  }
  // One page action in the world this gesture already shares with the page.
  CefRefPtr<CefDictionaryValue> PointerEvaluate(CefRefPtr<CefDictionaryValue> command,int context=-1) {
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",context>=0?context:world_);params->SetBool("returnByValue",true);params->SetBool("silent",true);
    params->SetBool("awaitPromise",true);
    return params;
  }
  CefRefPtr<CefDictionaryValue> PointerStep() const {
    auto steps=pending_ && pending_->command->GetType("steps")==VTYPE_LIST?pending_->command->GetList("steps"):nullptr;
    return steps && pointer_index_<steps->GetSize()?steps->GetDictionary(pointer_index_):CefRefPtr<CefDictionaryValue>();
  }
  // The frame a command names, as the outline prints it: the number alone,
  // or the whole of what the outline writes, `frame=1` or `[frame=1]`, so a
  // caller that copies what it read is understood.
  static std::string NamedFrame(CefRefPtr<CefDictionaryValue> command) {
    if(!command || !command->HasKey("frame")) return std::string();
    const auto value=command->GetValue("frame");
    std::string written;
    if(command->GetType("frame")==VTYPE_STRING) written=command->GetString("frame").ToString();
    else if(command->GetType("frame")==VTYPE_INT) written=std::to_string(command->GetInt("frame"));
    else if(command->GetType("frame")==VTYPE_DOUBLE) {
      const double number=command->GetDouble("frame");
      if(number>=1 && number<1e6 && number==std::floor(number)) written=std::to_string(static_cast<int64_t>(number));
    }
    // What the outline brackets is the id: `frame=1` and `[frame=12]` both
    // carry it, and the spaces around a copied id are not part of it.
    const auto trimmed=[](std::string text) {
      const auto first=text.find_first_not_of(" \t\n");
      const auto last=text.find_last_not_of(" \t\n");
      return first==std::string::npos?std::string():text.substr(first,last-first+1);
    };
    written=trimmed(trimmed(written));
    if(written.size()>=2 && written.front()=='[' && written.back()==']') written=trimmed(written.substr(1,written.size()-2));
    if(written.compare(0,6,"frame=")==0) written=trimmed(written.substr(6));
    return written.size()<=16?written:std::string();
  }
  void AdvancePointer() {
    if(!pending_) return;
    // Only a gesture has steps: a step still queued when the gesture settled
    // finds the read its settlement left, and has nothing left to place.
    auto steps=pending_->command->GetType("steps")==VTYPE_LIST?pending_->command->GetList("steps"):nullptr;
    if(!steps) return;
    // A gesture reads the page's tree as an action does: its own world is
    // made in the document the tree names.
    if(!tree_ || tree_generation_!=generation_) { CallInt(Gesture(GestureStage::Tree),"Page.getFrameTree",CefRefPtr<CefDictionaryValue>());return; }
    if(!world_) { BeginGestureWorld();return; }
    // The page hears that the gesture began, so the edits its steps cause
    // (a box its click ticks) are the gesture's, not a person's typing.
    if(!pending_->acting_told) {
      pending_->acting_told=true;
      auto command=CefDictionaryValue::Create();
      command->SetString("action","acting");command->SetInt("doc",document_);Mark(command);
      Ask(std::string(),"Runtime.evaluate",PointerEvaluate(command,world_),[this](bool,CefRefPtr<CefDictionaryValue>) { AdvancePointer(); });
      return;
    }
    if(pointer_index_>=steps->GetSize()) { SettlePointer();return; }
    auto step=steps->GetDictionary(pointer_index_);
    if(!step) { Fail("invalid_request","A gesture step is not an object");return; }
    step_located_=false;step_box_={0,0};
    if(!step->HasKey("element")) { PointerLocated();return; }
    const auto ref=step->GetString("element").ToString();
    // A step that keeps to the box a hand is already holding steps there: the
    // page does not re-place it, which would move the box mid-gesture.
    // A held box is a point of the page, so the place that moved it is not
    // added again.
    if(const auto held=HeldBox(ref)) { gesture_x_=0;gesture_y_=0;step_box_=*held;step_located_=true;PointerLocated();return; }
    // A step in a frame names an element of that frame's document, which the
    // page's own world does not hold. The ref says which document it was read
    // in, and a step may name the frame as well: the box is read in the
    // frame's own document and moved by where the frame sits in the page.
    const auto named=NamedFrame(step);
    if(!named.empty()) {
      const int handle=FrameHandle(named);
      if(!handle) { Fail("no_frame",NoSuchFrame(named));return; }
      EnterFrame(handle,[this,ref](const PageDocument& document) { StepInFrame(document.number,ref); },
        [this](const std::string& code,const std::string& message) { Fail(code,message); });
      return;
    }
    if(const auto held=docs_.find(RefDocument(ref));held!=docs_.end() && held->second.holder) { StepInFrame(held->first,ref);return; }
    SetGesture(0,0);
    auto command=CefDictionaryValue::Create();
    command->SetString("action","locate_box");command->SetInt("doc",document_);
    command->SetString("element",ref);Mark(command);
    CallInt(Gesture(GestureStage::Locate),"Runtime.evaluate",PointerEvaluate(command,world_));
  }
  // The world the gesture's own steps run in, made from the page's document.
  void BeginGestureWorld() {
    auto params=CefDictionaryValue::Create();
    params->SetString("frameId",main_id_);params->SetString("worldName","tilo-page-actions");
    CallInt(Gesture(GestureStage::World),"Page.createIsolatedWorld",params);
  }
  // The box of a step's element, read in the frame document that holds it,
  // and where that frame sits in the page: the two make one point of the
  // page, which is where the engine's pointer goes.
  void StepInFrame(int number,const std::string& ref) {
    PlaceDocument(number,true,[this,number,ref](Clicked place) {
      const auto held=docs_.find(number);
      if(held==docs_.end()) { Fail("frame_gone","The frame left the page during the gesture");return; }
      SetGesture(place.x,place.y);
      auto command=CefDictionaryValue::Create();
      command->SetString("action","locate_box");command->SetInt("doc",number);command->SetString("element",ref);Mark(command);
      if(held->second.handle) command->SetString("frame",std::to_string(held->second.handle));
      Ask(held->second.session,"Runtime.evaluate",PointerEvaluate(command,held->second.context),[this,number](bool success,CefRefPtr<CefDictionaryValue> result) {
        auto value=Said(success,result);
        if(!value) { Forget(number);Fail("stale_element","That element's frame moved on to another document during the gesture; read the page again");return; }
        if(value->GetType("ok")!=VTYPE_BOOL || !value->GetBool("ok")) {
          const auto code=value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"stale_element";
          Fail(code.size()<=64?code:"stale_element",Text(value,"message"));return;
        }
        auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
        if(!data) { Fail("script","The page gave no position for that element");return; }
        step_box_={Number(data,"x"),Number(data,"y")};step_located_=true;
        PointerLocated();
      });
    });
  }
  // The box a step resolved to, kept for the rest of its gesture.
  void RememberBox(const std::string& ref) {
    if(gesture_boxes_.size()>=kGestureBoxes) return;
    if(std::any_of(gesture_boxes_.begin(),gesture_boxes_.end(),[&](const HeldBoxEntry& held){return held.ref==ref;})) return;
    // A ref's box is remembered with the point it lies at in the page.
    gesture_boxes_.push_back({ref,step_box_.x+gesture_x_,step_box_.y+gesture_y_});
  }
  std::optional<Clicked> HeldBox(const std::string& ref) const {
    for(const auto& held:gesture_boxes_) {
      if(held.ref!=ref) continue;
      // The box was placed against the frame the step named, so it is held
      // where the page placed it, never moved twice.
      return Clicked{held.x,held.y};
    }
    return std::nullopt;
  }
  // A step naming an element is placed in that element's box; one that names
  // none is already a viewport position. A step naming a frame is placed in
  // that frame's place, the box measured inside the frame and moved by where
  // the frame sits: the two halves only the page's own processes can give.
  void PointerLocated() {
    auto step=PointerStep();
    double x=0,y=0;
    if(step->HasKey("element")) {
      if(!step_located_) { Fail("stale_element","That element is not on the page ("+step->GetString("element").ToString()+(step->HasKey("frame")?" frame "+NamedFrame(step):"")+")");return; }
      x=step_box_.x+gesture_x_+Number(step,"x");y=step_box_.y+gesture_y_+Number(step,"y");
      RememberBox(step->GetString("element").ToString());
    } else { x=Number(step,"x");y=Number(step,"y"); }
    step_located_=false;
    auto event=CefDictionaryValue::Create();
    event->SetString("kind",step->GetString("kind"));event->SetDouble("x",x);event->SetDouble("y",y);
    event->SetString("button",pending_->command->GetString("button"));
    pointer_(event);
    ++pointer_index_;
    // A gesture's steps are its own frames: the page sees each position it
    // would have seen from a hand moving across it.
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->AdvancePointer();
    },CefRefPtr<PageAutomation>(this),pending_->token),kPointerFrameMs);
  }
  // What the gesture did to the page is what the caller reads back: the same
  // read of the page any other action answers with, once the last step has
  // painted.
  void SettlePointer() {
    pointer_index_=0;gesture_=false;gesture_x_=gesture_y_=0;gesture_boxes_.clear();step_located_=false;
    // What the gesture did to the page is read like any other read: no page
    // code runs for it, and a gesture in a frame is not the page leaving.
    // The read is a `changes` command of its own: the gesture's own keys are
    // not part of it, so nothing the gesture carried is read as a read.
    auto read=CefDictionaryValue::Create();
    read->SetString("action","changes");
    read->SetString("op",pending_->command->GetString("op"));
    read->SetInt("request_id",pending_->request);
    read->SetInt("doc",document_);
    pending_->command=read;
    pending_->navigation="none";
    // A world asked for the gesture's own steps is not one an action's read
    // waits on, and a world still to be made for a frame's place is put off
    // until the next step needs it.
    world_request_.clear();
    // The gesture's own world is the page's own document, which is where an
    // action's read runs: the read is asked for there, in the world the
    // gesture left the page in.
    if(!main_world_ && !world_) {
      world_request_=main_id_;
      auto params=CefDictionaryValue::Create();
      params->SetString("frameId",main_id_);params->SetString("worldName","tilo-page-actions");
      Call(Stage::World,"Page.createIsolatedWorld",params);return;
    }
    if(!main_world_) main_world_=world_;
    Evaluate();
  }
  // The step of a gesture the page is asked for: what the page holds before
  // the pointer moves, or a point for it to resolve.
  enum class PageGesture { Begin, Landing, Path, End };
  // A gesture starts from what the page shows now, so its answer can say
  // what the pointer changed.
  void BeginPageGesture() {
    pending_->steps=std::max(1,drag_steps());
    // The page's own CSS decides whether a hover has to be mirrored: a page
    // whose hover reveals nothing is driven by the browser's pointer alone.
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run({action:'need_styles'})");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    CallIn(Stage::Hover,world_session_,"Runtime.evaluate",params);
  }
  int drag_steps() const {
    return pending_ && pending_->command->HasKey("steps")?static_cast<int>(pending_->command->GetInt("steps")):1;
  }
  // CSS pixels to the view's zoomed DIPs, where CEF's mouse events live.
  CefPoint view_point(double x,double y) const {
    const double zoom=std::pow(1.2,browser_->GetHost()->GetZoomLevel());
    return CefPoint(static_cast<int>(std::lround(x*zoom)),static_cast<int>(std::lround(y*zoom)));
  }
  bool gesture_page(CefRefPtr<CefDictionaryValue> value) const {
    return value && value->GetType("gesture_page")==VTYPE_BOOL && value->GetBool("gesture_page");
  }
  void GesturePage(Stage stage,PageGesture kind) {
    auto command=CefDictionaryValue::Create();
    const auto& source=pending_->command;
    command->SetInt("doc",document_);
    command->SetBool("gesture_page",true);
    if(!world_frame_.number.empty()) command->SetString("frame",world_frame_.number);
    Mark(command);
    // The page mirrors a hover through its own CSS only when that CSS opens
    // something, which it said before the gesture began.
    command->SetBool("mirror",pending_->hover_styles);
    // The gesture the page is asked for: a hover at a point travels as `at`
    // with `hover` set, so it is named here rather than taken from the action.
    const std::string gesture=HoverAtPoint()?"hover":Action();
    const std::string action=kind==PageGesture::Begin?"gesture_begin":kind==PageGesture::End?"gesture_end"
      :kind==PageGesture::Path?"path":gesture;
    command->SetString("action",action);
    command->SetString("kind",gesture);
    for(const char* key:{"element","to"}) if(source->HasKey(key)) command->SetString(key,source->GetString(key));
    for(const char* key:{"x","y","to_x","to_y"}) if(source->HasKey(key)) command->SetDouble(key,Number(source,key));
    // A path's answer says how many points it drew, which is what a canvas
    // holds and the outline cannot show.
    if(gesture=="path" && pending_->drawing) command->SetInt("drew",static_cast<int>(pending_->points.size()));
    // A hover's answer is read where the pointer was rested, which the page
    // only learns once its element has a centre.
    if(kind==PageGesture::End && gesture=="hover") {
      const double zoom=std::pow(1.2,browser_->GetHost()->GetZoomLevel());
      command->SetDouble("x",pending_->from_x/zoom-world_frame_.x);command->SetDouble("y",pending_->from_y/zoom-world_frame_.y);
    }
    // A drawing path carries its points; the page resolves them from the
    // element's own box, in viewport pixels for the engine's pointer.
    if((kind==PageGesture::Begin || kind==PageGesture::Path) && source->HasKey("points")) command->SetList("points",source->GetList("points")->Copy());
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    params->SetBool("awaitPromise",true);
    CallIn(stage,world_session_,"Runtime.evaluate",params);
  }
  // A turn of the wheel asks the page where the wheel turns and what it has
  // left to move, in the action's own world: the page resolves the element
  // the caller named, or the viewport, and the engine turns the wheel there.
  void BeginScroll() {
    scroll_=Scroll{};scroll_pending_=false;scroll_.began=Now();
    auto command=CefDictionaryValue::Create();
    const auto& source=pending_->command;
    command->SetString("action","scroll_target");command->SetInt("doc",document_);
    if(!world_frame_.number.empty()) command->SetString("frame",world_frame_.number);
    for(const char* key:{"element","into_view","to"}) if(source->HasKey(key)) command->SetString(key,source->GetString(key));
    if(source->HasKey("by")) command->SetInt("by",source->GetInt("by"));
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("silent",true);params->SetBool("awaitPromise",true);params->SetInt("timeout",5000);
    CallIn(Stage::Scroll,world_session_,"Runtime.evaluate",params);
  }
  // One more step of the wheel, after a frame: the page says what is left,
  // and the request it belongs to is still the one running.
  void ScrollCall(CefRefPtr<CefDictionaryValue> command,uint64_t token) {
    if(!pending_ || pending_->token!=token) return;
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("silent",true);params->SetBool("awaitPromise",true);params->SetInt("timeout",5000);
    CallIn(Stage::Scroll,world_session_,"Runtime.evaluate",params);
  }
  // One turn of the wheel, or the answer that there is nothing left to turn.
  // The wheel is real input at a point of the page, so the page's own scroll
  // listeners, intersection observers and lazy loading run for it, and the
  // page says after each turn how much of the distance is left.
  void ScrollTurn(CefRefPtr<CefDictionaryValue> value) {
    // A wheel whose page left the document under it has turned over nothing,
    // and a request is never left waiting for a turn that will not come.
    if(!value) { Fail("not_ready","The page left while the wheel was turning it");return; }
    if(value->GetType("ok")!=VTYPE_BOOL) { Fail("script","The page did not return a result");return; }
    if(!value->GetBool("ok")) {
      const auto code=value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"script";
      Fail(code.size()<=64?code:"script",Text(value,"message"));return;
    }
    const auto x=Number(value,"x"),y=Number(value,"y"),left=Number(value,"left");
    if(std::isfinite(x) && std::isfinite(y)) { scroll_.x=x;scroll_.y=y; }
    scroll_.left=std::isfinite(left)?left:0;
    // A page that still has something to give says so; the wheel turns again
    // once that has had a frame, as a hand's next turn would.
    if(scroll_.left==0 || scroll_.turns>=static_cast<int>(kMaxScrollTurns) || Now()-scroll_.began>=kScrollBudgetMs) {
      // Nothing more to do: the answer is what the page shows, as every other
      // action reads it back.
      WorldRead();return;
    }
    // The wheel turns by the distance that is left, signed: a positive one
    // carries the page down — its content up — and a negative one back up.
    // A turn the page has not shown yet is waited for, not made again: the
    // page says so, and is asked once more what is left.
    const bool wait=value->GetType("wait")==VTYPE_BOOL && value->GetBool("wait");
    if(!wait) {
      const auto turn=std::max(-kMaxScrollTurn,std::min(kMaxScrollTurn,scroll_.left));
      scroll_.left-=turn;++scroll_.turns;
      // A turn goes out as wheel events of at most one page of the box each,
      // one behind the other. Where the engine's wheel is a mouse's (Linux),
      // Chromium carries a box no further than a page of itself for the
      // wheel event that begins a scroll, however far the event says, and
      // carries the events behind it in full: a turn sent as one event
      // moves a short list one page and stops.
      const double said=Number(value,"page");
      const double page=said>=1?said:kMaxScrollTurn;
      double sent=0;
      for(int pieces=0;(pieces==0 || std::abs(turn-sent)>=0.5) && pieces<kMaxWheelPieces;++pieces) {
        const double piece=std::max(-page,std::min(page,turn-sent));
        // The wheel turns at a point of the page: a point of a frame's
        // document is moved by where the frame sits.
        wheel_(scroll_.x+world_frame_.x,scroll_.y+world_frame_.y,-piece);
        sent+=piece;
      }
    }
    auto command=CefDictionaryValue::Create();
    command->SetString("action","scroll_left");command->SetInt("doc",document_);
    // The page is asked what is left after the frame the wheel moved it in.
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,CefRefPtr<CefDictionaryValue> command,uint64_t token) {
      self->ScrollCall(command,token);
    },CefRefPtr<PageAutomation>(this),command,pending_->token),kScrollWheelMs);
  }
  // What the page shows once the wheel has stopped, read like any other
  // action: the outline lines the turn added and removed.
  void WorldRead() {
    auto read=CefDictionaryValue::Create();
    read->SetString("action","changes");
    read->SetString("op",pending_->command->GetString("op"));
    read->SetInt("request_id",pending_->request);
    read->SetInt("doc",document_);
    pending_->command=read;
    pending_->navigation="none";
    // The document whose changing the caller waits to hear about is the one
    // the read runs in: a navigation that began while the action waited means
    // a new document is here, so what the action changed is unknown.
    pending_->waits_on_document=true;
    world_request_.clear();
    if(!main_world_ && !world_) {
      world_request_=main_id_;
      auto params=CefDictionaryValue::Create();
      params->SetString("frameId",main_id_);params->SetString("worldName","tilo-page-actions");
      Call(Stage::World,"Page.createIsolatedWorld",params);return;
    }
    if(!main_world_) main_world_=world_;
    Evaluate();
  }
  // Where the frame tree leads: the document this action runs in. The tree is
  // read once per page generation, so repeated actions on one page do not each
  // pay for it and none of them acts on a tree an older document described.
  void AfterTree() {
    auto frame=tree_?tree_->GetDictionary("frame"):nullptr;
    if(!frame) { Fail("unavailable","The page has no document to read");return; }
    if(PrivateText()) {
      std::vector<TextFrame> frames;
      if(!CollectTextFrames(tree_,frames)) { Fail("unavailable","The frame tree cannot be bounded and observed");return; }
      if(Action()=="prepare_text_input") {
        text_candidates_=std::move(frames);ProbeTextFrame();return;
      }
      const auto lease=pending_->command->GetString("lease").ToString();
      if(!text_owner_ || lease!=text_lease_) { Fail("text_input_retired","The original frame lease is no longer active");return; }
      const auto owner=*text_owner_;
      const bool live=std::any_of(frames.begin(),frames.end(),[&](const TextFrame& candidate) {
        return candidate.id==owner.id && candidate.loader==owner.loader;
      });
      if(!live) { text_owner_.reset();text_lease_.clear();Fail("text_input_retired","The original frame document was replaced");return; }
      text_frame_=owner;Evaluate();return;
    }
    // The document this action runs in. A frame the caller named, by the
    // number the outline printed for it, is found again from the element that
    // holds it. An element's ref says which document it was read in, so an
    // action on an element of a frame runs in that frame without naming it.
    // Anything else runs in the page's own document. A number no read of this
    // tab printed is refused rather than read as the page, so a caller never
    // reads the wrong document believing it read the frame.
    const auto named=NamedFrame(pending_->command);
    if(!named.empty()) {
      const int handle=FrameHandle(named);
      if(!handle) { Fail("no_frame",NoSuchFrame(named));return; }
      // The document last entered for the frame answers while it is there;
      // once it is not, the frame is found again from its element.
      for(const auto& [number,document]:docs_) if(document.handle==handle) { const PageDocument held=document;RunIn(held);return; }
      EnterFrame(handle,[this](const PageDocument& document) { RunIn(document); },
        [this](const std::string& code,const std::string& message) { Fail(code,message); });
      return;
    }
    for(const char* key:{"element","into_view"}) {
      if(pending_->command->GetType(key)!=VTYPE_STRING) continue;
      const auto held=docs_.find(RefDocument(pending_->command->GetString(key).ToString()));
      if(held!=docs_.end() && held->second.holder) { const PageDocument document=held->second;RunIn(document);return; }
      break;
    }
    PageFrame target;
    target.id=frame->GetString("id").ToString();target.loader=frame->GetString("loaderId").ToString();
    target.address=frame->GetType("url")==VTYPE_STRING?frame->GetString("url").ToString():std::string();
    // A turn of the wheel says where it turns before the page is asked
    // anything: the box it signs into the page's script is the one the caller
    // named.
    if(Action()=="scroll") {
      // A context left by an earlier action is reused, and a document that
      // was replaced gets a new one, so the wheel never turns in a stale
      // document.
      if(world_id_==target.id && world_ && target.loader==loader_) { BeginScroll();return; }
      scroll_pending_=true;
      // The page's own frame tree names the document, but it is a stale copy
      // once the page it describes has gone: the world then has to be made
      // again for the new document, not the one the copy remembers.
      tree_.reset();tree_generation_=0;
    }
    if(world_id_==target.id && world_ && target.loader==loader_) { Evaluate();return; }
    loader_=target.loader;world_=0;world_frame_=target;
    auto params=CefDictionaryValue::Create();
    params->SetString("frameId",target.id);params->SetString("worldName","tilo-page-actions");
    Call(Stage::World,"Page.createIsolatedWorld",params);
  }
  // The action runs in a frame's own document, on the connection that frame
  // answers on: everything the page's own document is asked, the frame's is
  // asked there.
  void RunIn(const PageDocument& document) {
    world_=document.context;document_=document.number;world_session_=document.session;frame_doc_=document.number;
    world_id_=document.frame;world_frame_=PageFrame{};world_frame_.id=document.frame;
    world_frame_.number=std::to_string(document.handle);
    pending_->frame=document.handle;
    // The wheel turns at a point of the page, so a turn in a frame first
    // learns where the frame sits.
    if(Action()=="scroll") {
      PlaceDocument(document.number,true,[this](Clicked place) { world_frame_.x=place.x;world_frame_.y=place.y;BeginScroll(); });
      return;
    }
    Evaluate();
  }
  // Read the page's frames again: a page attaches, detaches and navigates its
  // frames after it loaded, and the tree says which document is the page's
  // own now.
  void ReadTree() { Call(Stage::FrameTree,"Page.getFrameTree",nullptr); }
  // One question to the engine on the way to an action's answer, and what to
  // do with what it says: the steps that find a frame, enter it and place it
  // are asked one after another this way.
  using Answer=std::function<void(bool,CefRefPtr<CefDictionaryValue>)>;
  void Ask(const std::string& session,const char* method,CefRefPtr<CefDictionaryValue> params,Answer then) {
    asked_=std::move(then);
    CallIn(Stage::Asked,session,method,params);
  }
  // What the page's script answered an evaluation with, or nothing when the
  // document it was asked in is gone.
  static CefRefPtr<CefDictionaryValue> Said(bool success,CefRefPtr<CefDictionaryValue> result) {
    auto remote=success && result?result->GetDictionary("result"):nullptr;
    return remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
  }
  // Said to a connection without waiting for what it answers.
  void Tell(const std::string& session,const char* method,CefRefPtr<CefDictionaryValue> params) {
    if(!browser_) return;
    if(session.empty()) { browser_->GetHost()->ExecuteDevToolsMethod(0,method,params);return; }
    const auto wire=Wire(NextSessionCall(),session,method,params);
    browser_->GetHost()->SendDevToolsMessage(wire.c_str(),wire.size());
  }
  static std::string Wire(int id,const std::string& session,const char* method,CefRefPtr<CefDictionaryValue> params) {
    auto message=CefDictionaryValue::Create();
    message->SetInt("id",id);message->SetString("method",method);message->SetString("sessionId",session);
    if(params) message->SetDictionary("params",params);
    auto value=CefValue::Create();value->SetDictionary(message);
    return CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString();
  }
  int NextSessionCall() { return next_message_=next_message_>=kLastSessionCall?kFirstSessionCall+1:next_message_+1; }
  // A frame of another site runs in a process of its own, which the page's
  // inspector reaches by a connection of its own: it is asked to hand those
  // connections out as such frames come, for frames only, and without
  // holding a frame's document back while it does.
  static CefRefPtr<CefDictionaryValue> AutoAttach() {
    auto params=CefDictionaryValue::Create();
    params->SetBool("autoAttach",true);params->SetBool("waitForDebuggerOnStart",false);params->SetBool("flatten",true);
    auto frames=CefDictionaryValue::Create();frames->SetString("type","iframe");
    auto filter=CefListValue::Create();filter->SetDictionary(0,frames);
    params->SetList("filter",filter);
    return params;
  }
  // A connection the inspector handed out for a frame of another site. The
  // target it names is the frame itself, by the id the engine gives the
  // frame wherever it is asked, so the connection is found from the frame.
  void TargetAttached(CefRefPtr<CefDictionaryValue> params) {
    auto info=params?params->GetDictionary("targetInfo"):nullptr;
    if(!info || params->GetType("sessionId")!=VTYPE_STRING || info->GetType("targetId")!=VTYPE_STRING) return;
    if(info->GetType("type")==VTYPE_STRING && info->GetString("type")!="iframe") return;
    const auto session=params->GetString("sessionId").ToString(),target=info->GetString("targetId").ToString();
    if(target_sessions_.size()>=kMaxFrameSessions && !target_sessions_.count(target)) return;
    target_sessions_[target]=session;session_targets_[session]=target;
    // A frame of another site may hold frames of yet another: its own
    // connection hands those out.
    Tell(session,"Target.setAutoAttach",AutoAttach());
  }
  // A connection taken back: the frame left, or moved to another process.
  // Every document read on it is gone, and a call waiting on it is answered
  // as its document having gone, never left waiting.
  void TargetDetached(CefRefPtr<CefDictionaryValue> params) {
    if(!params || params->GetType("sessionId")!=VTYPE_STRING) return;
    const auto session=params->GetString("sessionId").ToString();
    if(const auto held=session_targets_.find(session);held!=session_targets_.end()) {
      const auto target=target_sessions_.find(held->second);
      if(target!=target_sessions_.end() && target->second==session) target_sessions_.erase(target);
      session_targets_.erase(held);
    }
    std::vector<int> gone;
    for(const auto& [number,document]:docs_) if(document.session==session) gone.push_back(number);
    for(const int number:gone) Forget(number);
    if(pending_ && call_ && call_session_==session)
      ApplyAnswer(call_,false,"{\"message\":\"Execution context was destroyed: the frame's connection closed\"}");
  }
  // The number of the document a script context holds: the one it was given
  // while this side knew it, or a new one. A document is numbered once, so a
  // ref keeps naming the document it was read in.
  int DocumentOf(const std::string& session,int context) {
    for(const auto& [number,document]:docs_) if(document.context==context && document.session==session) return number;
    return page_documents=page_documents%2147483647+1;
  }
  // The page's own document, in the world made for it.
  void EnterMain(int context) {
    world_=context;main_world_=context;world_session_.clear();frame_doc_=0;
    Tell(std::string(),"Runtime.addBinding",NoteBinding(context));
    document_=DocumentOf(std::string(),context);main_document_=document_;
    PageDocument page;page.number=document_;page.context=context;page.frame=main_id_;
    docs_[document_]=page;
  }
  // A document that is gone, with every frame document it held.
  void Forget(int number) {
    std::vector<int> gone{number};
    for(size_t i=0;i<gone.size();++i)
      for(const auto& [held,document]:docs_) if(document.holder==gone[i]) gone.push_back(held);
    for(const int each:gone) docs_.erase(each);
    if(frame_doc_ && !docs_.count(frame_doc_)) LeaveFrame();
  }
  // Nothing of the page that was: its documents, its worlds and the tree
  // that named them belong to a document the page has left.
  void ForgetPage() {
    docs_.clear();contexts_.clear();world_=0;main_world_=0;main_document_=0;frame_doc_=0;
    world_session_.clear();world_id_.clear();world_frame_=PageFrame{};world_request_.clear();
    tree_=nullptr;tree_page_.clear();tree_loader_.clear();
  }
  // Back to the page's own document, after an action in a frame's.
  void LeaveFrame() {
    if(!frame_doc_) return;
    frame_doc_=0;world_session_.clear();world_=main_world_;document_=main_document_;
    world_id_=main_world_?main_id_:std::string();world_frame_=PageFrame{};world_frame_.id=world_id_;
  }
  // The number the page's script gave the document it answered from. A
  // script that outlived what this side knew of its document keeps its own
  // number, and that is the one its refs carry.
  void Adopt(CefRefPtr<CefDictionaryValue> value) {
    if(!value || PrivateText() || value->GetType("doc")!=VTYPE_INT) return;
    const int said=value->GetInt("doc"),held=document_;
    if(said<=0 || said==held || docs_.count(said)) return;
    auto document=docs_.extract(held);
    if(document.empty()) return;
    document.key()=said;document.mapped().number=said;
    docs_.insert(std::move(document));
    for(auto& [number,other]:docs_) if(other.holder==held) other.holder=said;
    if(main_document_==held) main_document_=said;
    if(frame_doc_==held) frame_doc_=said;
    else if(contexts_.count(world_)) contexts_[world_]=said;
    document_=said;
  }
  // The frame a number names, entered: the document it holds now, in a world
  // of this side's own. The element that holds the frame is asked for in the
  // document that printed it, the engine says which frame that element holds
  // (its own answer, for a frame of any site), and the world is made on the
  // connection that frame answers on. Nothing here reaches into a frame from
  // the page around it: the frame's own process is asked, as the page's is.
  using Entered=std::function<void(const PageDocument&)>;
  using Missed=std::function<void(const std::string&,const std::string&)>;
  void EnterFrame(int handle,Entered entered,Missed missed) {
    const auto named=frame_refs_.find(handle);
    if(named==frame_refs_.end()) { missed("no_frame",NoSuchFrame(std::to_string(handle)));return; }
    const std::string ref=named->second;
    const auto holder=docs_.find(RefDocument(ref));
    if(holder==docs_.end()) { missed("frame_gone",FrameGone(handle));return; }
    const PageDocument held=holder->second;
    if(!auto_attach_) {
      auto_attach_=true;
      Ask(std::string(),"Target.setAutoAttach",AutoAttach(),[this,handle,entered,missed](bool,CefRefPtr<CefDictionaryValue>) {
        EnterFrame(handle,entered,missed);
      });
      return;
    }
    auto quoted=CefValue::Create();quoted->SetString(ref);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression","globalThis.__tiloPage?globalThis.__tiloPage.frameNode("+CefWriteJSON(quoted,JSON_WRITER_DEFAULT).ToString()+"):null");
    params->SetInt("contextId",held.context);params->SetString("objectGroup","tilo-page-frames");params->SetBool("silent",true);
    Ask(held.session,"Runtime.evaluate",params,[this,handle,held,entered,missed](bool success,CefRefPtr<CefDictionaryValue> result) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      // The document that listed the frame is gone, and the frame with it.
      if(!remote) { Forget(held.number);missed("frame_gone",FrameGone(handle));return; }
      if(remote->GetType("objectId")!=VTYPE_STRING) { missed("frame_gone",FrameGone(handle));return; }
      auto node=CefDictionaryValue::Create();node->SetString("objectId",remote->GetString("objectId"));
      Ask(held.session,"DOM.describeNode",node,[this,handle,held,entered,missed](bool success,CefRefPtr<CefDictionaryValue> result) {
        auto described=success && result?result->GetDictionary("node"):nullptr;
        const auto frame=described && described->GetType("frameId")==VTYPE_STRING?described->GetString("frameId").ToString():std::string();
        auto group=CefDictionaryValue::Create();group->SetString("objectGroup","tilo-page-frames");
        Tell(held.session,"Runtime.releaseObjectGroup",group);
        if(frame.empty()) { missed("frame_gone",FrameGone(handle));return; }
        FrameWorld(handle,held,frame,Now()+kFrameWaitMs,entered,missed);
      });
    });
  }
  void FrameWorld(int handle,const PageDocument& held,const std::string& frame,double deadline,Entered entered,Missed missed) {
    // A frame of another site runs in a process of its own and answers on a
    // connection of its own; any other frame answers where its holder does.
    const auto own=target_sessions_.find(frame);
    const std::string session=own!=target_sessions_.end()?own->second:held.session;
    auto params=CefDictionaryValue::Create();
    params->SetString("frameId",frame);params->SetString("worldName","tilo-page-actions");
    Ask(session,"Page.createIsolatedWorld",params,[this,handle,held,frame,session,deadline,entered,missed](bool success,CefRefPtr<CefDictionaryValue> result) {
      if(success && result && result->GetType("executionContextId")==VTYPE_INT) {
        PageDocument document;
        document.context=result->GetInt("executionContextId");document.session=session;document.frame=frame;
        Tell(session,"Runtime.addBinding",NoteBinding(document.context));
        document.holder=held.number;document.handle=handle;
        if(const auto named=frame_refs_.find(handle);named!=frame_refs_.end()) document.element=named->second;
        // A frame holds one document: what was kept of the one before it is
        // gone with it.
        std::vector<int> before;
        for(const auto& [number,other]:docs_)
          if(other.handle==handle && !(other.context==document.context && other.session==session)) before.push_back(number);
        for(const int number:before) Forget(number);
        document.number=DocumentOf(session,document.context);
        docs_[document.number]=document;
        entered(document);return;
      }
      // The frame is between two processes, or the connection to its own has
      // not been handed out yet: it is waited for, briefly, where it is.
      if(Now()>=deadline) { missed("not_ready","Frame "+std::to_string(handle)+" is still loading its document; ask again in a moment");return; }
      CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token,std::function<void()> again) {
        if(self->pending_ && self->pending_->token==token) again();
      },CefRefPtr<PageAutomation>(this),pending_->token,
        std::function<void()>([this,handle,held,frame,deadline,entered,missed] { FrameWorld(handle,held,frame,deadline,entered,missed); })),kFrameWaitStepMs);
    });
  }
  // Where a frame's document sits in the page: the place of its frame in the
  // document that holds it, and of that document's frame in turn, each
  // measured by the document that holds the element and summed out to the
  // page's own.
  void PlaceDocument(int number,bool anchored,std::function<void(Clicked)> then,Clicked sum={}) {
    const auto found=docs_.find(number);
    if(found==docs_.end()) { Fail("frame_gone","The frame left the page before it could be placed in it");return; }
    if(!found->second.holder) { then(sum);return; }
    const auto holder=docs_.find(found->second.holder);
    if(holder==docs_.end()) { Fail("frame_gone","The frame left the page before it could be placed in it");return; }
    auto command=CefDictionaryValue::Create();
    command->SetString("action","frame_offset");command->SetString("element",found->second.element);
    command->SetBool("anchored",anchored);command->SetInt("doc",holder->first);
    if(holder->second.handle) command->SetString("frame",std::to_string(holder->second.handle));
    const int above=holder->first;
    Ask(holder->second.session,"Runtime.evaluate",PointerEvaluate(command,holder->second.context),[this,above,anchored,then,sum](bool success,CefRefPtr<CefDictionaryValue> result) {
      auto value=Said(success,result);
      if(!value || value->GetType("ok")!=VTYPE_BOOL || !value->GetBool("ok")) {
        Fail(value && value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"frame_gone",
          value && value->GetType("message")==VTYPE_STRING?value->GetString("message").ToString()
            :"The frame left the page before it could be placed in it");return;
      }
      PlaceDocument(above,anchored,then,{sum.x+Number(value,"x"),sum.y+Number(value,"y")});
    });
  }
  // One read in the document a frame holds now, for an action of the page
  // around it: what the frame's document said, or nothing when it could not
  // be read (the frame left, or is between two documents).
  void AskFrame(int handle,CefRefPtr<CefDictionaryValue> command,std::function<void(CefRefPtr<CefDictionaryValue>)> then,bool again=false) {
    const Entered ask=[this,handle,command,then,again](const PageDocument& document) {
      auto body=command->Copy(false);
      body->SetInt("doc",document.number);body->SetString("frame",std::to_string(handle));Mark(body);
      const int number=document.number;
      Ask(document.session,"Runtime.evaluate",PointerEvaluate(body,document.context),[this,handle,command,then,again,number](bool success,CefRefPtr<CefDictionaryValue> result) {
        if(auto value=Said(success,result)) { then(value);return; }
        Forget(number);
        if(again) { then(nullptr);return; }
        AskFrame(handle,command,then,true);
      });
    };
    if(!again) for(const auto& [number,document]:docs_) if(document.handle==handle) { const PageDocument held=document;ask(held);return; }
    EnterFrame(handle,ask,[then](const std::string&,const std::string&) { then(nullptr); });
  }
  // The tree read now says whether the page's own document is the one the
  // tree before it named: a page whose own document was replaced is read
  // afresh rather than acted on in the document that is gone.
  bool TreeMoved(CefRefPtr<CefDictionaryValue> tree) const {
    auto now=tree?tree->GetDictionary("frame"):nullptr;
    if(!now || now->GetType("id")!=VTYPE_STRING || now->GetType("loaderId")!=VTYPE_STRING) return true;
    return tree_page_.empty() || now->GetString("id").ToString()!=tree_page_ || now->GetString("loaderId").ToString()!=tree_loader_;
  }
  // A frame the page attached, detached or navigated since the tree was read:
  // the tree kept from then names frames that are gone and misses the ones
  // that are here, so the next action reads the page's frames again.
  void FramesMoved() { ++generation_; }
  // The page's frames moved in the inspector, or the agent's own from them:
  // either way the tree kept is older than the page.
  void OnDevToolsEvent(CefRefPtr<CefBrowser>,const CefString& method,const void* params,size_t size) override {
    const std::string name=method.ToString();
    // What the page's own world tells without being asked (see `Noted`).
    if(name=="Runtime.bindingCalled") {
      auto parsed=CefParseJSON(std::string(size>0?static_cast<const char*>(params):"",size>0?size:0),JSON_PARSER_RFC);
      Noted(std::string(),parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr);
      return;
    }
    // A frame of another site is a target of its own, and the page's
    // inspector says when a connection to one is handed out or taken back.
    if(name=="Target.attachedToTarget" || name=="Target.detachedFromTarget") {
      auto parsed=CefParseJSON(std::string(size>0?static_cast<const char*>(params):"",size>0?size:0),JSON_PARSER_RFC);
      auto said=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
      if(name=="Target.attachedToTarget") TargetAttached(said);else TargetDetached(said);
      return;
    }
    if(name=="Page.frameAttached" || name=="Page.frameDetached") { FramesMoved();return; }
    if(name!="Page.frameNavigated") return;
    // Its own document leaving is a page navigation, which the host's fences
    // already name; a frame's document, or the same one again, only moves the
    // frames an action can name.
    auto parsed=CefParseJSON(std::string(size>0?static_cast<const char*>(params):"",size>0?size:0),JSON_PARSER_RFC);
    auto params_frame=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    auto frame=params_frame?params_frame->GetDictionary("frame"):nullptr;
    const bool own=frame && frame->GetType("parentId")!=VTYPE_STRING;
    if(!own) { FramesMoved();return; }
    // The same document loaded again is no new document.
    auto leaf=tree_?tree_->GetDictionary("frame"):nullptr;
    const auto loader=frame->GetType("loaderId")==VTYPE_STRING?frame->GetString("loaderId").ToString():std::string();
    if(!leaf || loader.empty() || leaf->GetType("loaderId")!=VTYPE_STRING || leaf->GetString("loaderId").ToString()!=loader)
      FramesMoved();
  }
  void Call(Stage stage,const char* method,CefRefPtr<CefDictionaryValue> params) {
    CallInt(static_cast<int>(stage),method,params);
  }
  bool At(Stage stage) const { return stage_==static_cast<int>(stage); }
  void CallInt(int stage,const char* method,CefRefPtr<CefDictionaryValue> params) {
    stage_=stage;call_session_.clear();
    const int id=browser_->GetHost()->ExecuteDevToolsMethod(0,method,params);
    call_=id;stage_at_call_=stage;
    if(!call_) Fail("unavailable","The page's inspector connection is unavailable");
  }
  // A method sent to the page's own connection, or to the one a frame of
  // another site answers on: that frame runs a document of its own in a
  // process of its own, and only a connection of its own reaches it. Such a
  // call is sent as the wire's own message, which is where a connection is
  // named, and it is the call the action waits on like any other.
  void CallIn(Stage stage,const std::string& session,const char* method,CefRefPtr<CefDictionaryValue> params) {
    if(session.empty()) { Call(stage,method,params);return; }
    const int id=NextSessionCall();
    const auto wire=Wire(id,session,method,params);
    stage_=static_cast<int>(stage);stage_at_call_=stage_;call_=id;call_session_=session;
    if(!browser_->GetHost()->SendDevToolsMessage(wire.c_str(),wire.size())) {
      call_=0;Fail("unavailable","The frame's inspector connection is unavailable");
    }
  }
  void AdvanceStage(bool success,const std::string& json) {
    // A frame's settle answers from the frame's own document, which the click
    // may have replaced: that is the frame leaving, not the page.
    if(At(Stage::FrameSettle)) { FrameSettled(success,json);return; }
    auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
    auto result=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    // A question asked on the way to the action's answer is answered where
    // it was asked (see `Ask`).
    if(At(Stage::Asked)) {
      auto then=std::move(asked_);asked_=nullptr;
      if(then) then(success,result);
      return;
    }
    // An action in a frame's document that is answered with nothing lost that
    // document: the frame moved on to another, or left the page. What was
    // kept of the document is forgotten and the action is read once more, so
    // one that names the frame runs in the document the frame holds now, and
    // one whose ref was read in the lost document is told the ref is stale.
    if(frame_doc_ && (At(Stage::Evaluate) || At(Stage::Scroll)) &&
       (!success || !result || (result->GetType("result")!=VTYPE_DICTIONARY && result->GetType("exceptionDetails")!=VTYPE_DICTIONARY))) {
      const bool evaluating=At(Stage::Evaluate);
      Forget(frame_doc_);LeaveFrame();
      if(evaluating) {
        if(pending_->retried) { Fail("page_changed","The frame's document changed while the action ran in it; read the page again");return; }
        pending_->retried=true;Begin();return;
      }
      Fail("not_ready","The frame left its document while the wheel was turning it");return;
    }
    // A script context that answers with neither a result nor an exception has
    // gone with its document, which happens when a document is replaced while
    // it is being read. The request is read again in a fresh context, once per
    // request; a page that moved on still fails as changed.
    const bool scripted=At(Stage::Evaluate) || At(Stage::GestureBegin) || At(Stage::Path) ||
      At(Stage::Hover) || At(Stage::Pointer) || At(Stage::GestureEnd) || At(Stage::Scroll);
    if(scripted && world_ && !frame_doc_ && (!success || !result || (result->GetType("result")!=VTYPE_DICTIONARY && result->GetType("exceptionDetails")!=VTYPE_DICTIONARY))) {
      const auto document=contexts_.count(world_)?contexts_[world_]:0;
      contexts_.erase(world_);world_=0;
      if(document!=document_ && !pending_->retried) { pending_->retried=true;Begin();return; }
    }
    // The engine painted the area the page named: the picture is the answer.
    if(At(Stage::Paint)) { Painted(success,result);return; }
    // The answer is handled where CEF delivered it, never from inside this
    // call's own dispatch: every method below is sent after the handler runs.
    if(At(Stage::NativeElement) || At(Stage::NativeDOM) || At(Stage::NativeDocument) ||
        At(Stage::NativeNode) || At(Stage::NativeEditors) || At(Stage::NativeResolve) || At(Stage::NativeConfigure) ||
        At(Stage::FileTarget) || At(Stage::FileSet)) {
      NativeResult(success,result);return;
    }
    if(At(Stage::TextFailureFrameTree)) {
      std::vector<TextFrame> frames;
      auto tree=success && result?result->GetDictionary("frameTree"):nullptr;
      if(text_owner_ && CollectTextFrames(tree,frames)) {
        const auto owner=*text_owner_;
        const bool live=std::any_of(frames.begin(),frames.end(),[&](const TextFrame& candidate) {
          return candidate.id==owner.id && candidate.loader==owner.loader;
        });
        if(!live) { Fail("text_input_retired","The original checked frame was replaced");return; }
      }
      Fail("unavailable","The original text request was not acknowledged");return;
    }
    if(stage_>=kGestureBase) {
      if(stage_==Gesture(GestureStage::Tree)) {
        // A frame tree names the document a fresh world belongs to, and the
        // frames the gesture's steps can name by their place in it.
        auto tree=success && result?result->GetDictionary("frameTree"):nullptr;
        auto frame=tree?tree->GetDictionary("frame"):nullptr;
        if(!frame || frame->GetType("id")!=VTYPE_STRING || frame->GetType("loaderId")!=VTYPE_STRING) { Fail("unavailable","The page has no document to read");return; }
        if(TreeMoved(tree)) ForgetPage();
        tree_=tree->Copy(true);tree_generation_=generation_;
        // The page's own document is the one a step is placed in: its world
        // comes from the tree read here.
        main_id_=frame->GetString("id").ToString();
        const auto loader=frame->GetString("loaderId").ToString();
        tree_page_=main_id_;tree_loader_=loader;
        if(world_ && loader==world_loader_) { AdvancePointer();return; }
        main_world_=0;
        loader_=loader;world_=0;
        // A gesture that aims into a frame measures the frame's place in the
        // page's own world, which is made with the frame tree it comes from.
        BeginGestureWorld();return;
      }
      if(stage_==Gesture(GestureStage::World)) {
        if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) { Fail("unavailable","The page has no document to read");return; }
        // The world belongs to the document the tree named: another document
        // is another world, however the ids of the two read.
        EnterMain(result->GetInt("executionContextId"));world_id_=main_id_;world_loader_=loader_;
        AdvancePointer();return;
      }
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
      if(!value) {
        // The document this step named is gone: place the step in the page
        // that replaced it, once, before answering that the ref is stale.
        if(pending_->retried) { Fail("page_changed","The page changed during the gesture");return; }
        pending_->retried=true;world_=0;
        CallInt(Gesture(GestureStage::Tree),"Page.getFrameTree",CefRefPtr<CefDictionaryValue>());return;
      }
      if(!value->GetBool("ok")) {
        const auto code=value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"script";
        Fail(code.size()<=64?code:"script",Text(value,"message"));return;
      }
      auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
      if(!data) { Fail("script","The page gave no position for that element");return; }
      // The box a step names is the input to its own placement: it is carried
      // to the point the step resolves to, not kept for a later one.
      step_box_={Number(data,"x"),Number(data,"y")};step_located_=true;
      PointerLocated();return;
    }
    if(At(Stage::FrameTree)) {
      auto frame=success && result?result->GetDictionary("frameTree"):nullptr;
      frame=frame?frame->GetDictionary("frame"):nullptr;
      if(!frame || frame->GetType("id")!=VTYPE_STRING || frame->GetType("loaderId")!=VTYPE_STRING) {
        Fail("unavailable","The page has no document to read");return;
      }
      main_id_=frame->GetString("id").ToString();
      // CEF owns a result value only for the callback that received it, so the
      // tree is copied before it is kept for the next action. The copy keeps
      // the whole tree, not only the node asked for, so a later action still
      // finds the page's own document in it.
      auto tree=result->GetDictionary("frameTree");
      // The page's own document may have been replaced since the tree kept: an
      // action read then runs in the document it names, not in a world that
      // belonged to the one before it, and nothing of that one is kept.
      if(TreeMoved(tree)) ForgetPage();
      tree_=tree->Copy(true);tree_generation_=generation_;
      tree_page_=main_id_;tree_loader_=frame->GetString("loaderId").ToString();
      // The tree leads straight on: which document this action runs in is the
      // next step, never the end of the action.
      AfterTree();return;
    } else if(At(Stage::TextWorld)) {
      if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) { ProbeTextFrame();return; }
      text_frame_.world=result->GetInt("executionContextId");
      text_frame_.document=page_documents=page_documents%2147483647+1;
      auto params=CefDictionaryValue::Create();
      // Observe focus only. No transcript or field content is extracted here.
      params->SetString("expression","document.hasFocus() && (()=>{let e=document.activeElement;while(e?.shadowRoot?.activeElement)e=e.shadowRoot.activeElement;return !!e && e.localName!=='iframe' && e.localName!=='frame';})()");
      params->SetInt("contextId",text_frame_.world);params->SetBool("returnByValue",true);params->SetBool("silent",true);
      Call(Stage::TextFocus,"Runtime.evaluate",params);
    } else if(At(Stage::TextFocus)) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      if(!remote || remote->GetType("value")!=VTYPE_BOOL || !remote->GetBool("value")) { ProbeTextFrame();return; }
      Evaluate();
    } else if(At(Stage::World)) {
      if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) { WorldRefused();return; }
      // The world an action asks for here is the page's own: a frame's is
      // made where the frame is found (see `EnterFrame`).
      world_request_.clear();
      EnterMain(result->GetInt("executionContextId"));
      world_id_=main_id_;world_frame_.id=main_id_;world_frame_.number.clear();world_frame_.x=world_frame_.y=0;
      if(!PrivateText()) contexts_[world_]=document_;
      if(Gesturing()) { BeginGesture();return; }
      // A turn of the wheel waited for a world to turn in: it turns now, and
      // only here is the wheel's own world known to be this action's.
      if(scroll_pending_) { scroll_pending_=false;BeginScroll();return; }
      Evaluate();
    } else if(At(Stage::Fence)) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
      // No answer means the document went away under the fence: a navigation
      // that began during the action has committed.
      Fenced(value?Text(value,"navigation"):navigations_!=pending_->navigations?"started":"possible");
    } else if(At(Stage::Pointer)) {
      // Where the page's own pointer rests is told around the gesture's own
      // step, which drives what happens next; its answer ends nothing.
      return;
    } else if(At(Stage::Scroll)) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
      ScrollTurn(value);return;
    } else if(At(Stage::GestureBegin) || At(Stage::Hover) || At(Stage::GestureEnd) || At(Stage::Path)) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      auto value=remote?remote->GetDictionary("value"):nullptr;
      // A frame that answers nothing has moved on to another document, and
      // the element the gesture named went with the one before.
      if(!value && frame_doc_) {
        Forget(frame_doc_);
        Fail("stale_element","That element's frame moved on to another document; read the page again");return;
      }
      if(!value || value->GetType("ok")!=VTYPE_BOOL) { Fail("script","The page did not return a result");return; }
      if(!value->GetBool("ok")) {
        const auto code=value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"script";
        Fail(code.size()<=64?code:std::string("script"),Text(value,"message"));return;
      }
      if(At(Stage::Hover)) {
        // The stylesheet answer decides whether this page's hover has to be
        // mirrored, and the gesture resolves its ends next.
        pending_->hover_styles=value->GetType("need_styles")==VTYPE_BOOL && value->GetBool("need_styles");
        GesturePage(Stage::GestureBegin,PageGesture::Begin);return;
      }
      if(value->GetType("document_url")==VTYPE_STRING) pending_->frame_url=Clip(value->GetString("document_url").ToString(),300);
      if(At(Stage::GestureBegin)) {
        // The ends of a gesture in a frame are points of the frame's own
        // document. The frame is brought into view and placed once the
        // gesture's element has been, as a click in a frame is, and the
        // pointer goes where the two put the point in the page.
        if(frame_doc_) {
          auto landed=value->Copy(false);
          PlaceDocument(frame_doc_,false,[this,landed](Clicked) {
            PlaceDocument(frame_doc_,true,[this,landed](Clicked place) { world_frame_.x=place.x;world_frame_.y=place.y;GestureLanded(landed); });
          });
          return;
        }
        GestureLanded(value);return;
      }
      if(At(Stage::Path)) { StartPath(value);return; }
      GestureFinished(value);
    } else {
      if(!success) {
        if(PrivateText()) {
          // A failed non-editing check may observe retirement, never replay an
          // evaluation or retarget. An ambiguous commit stays unknown.
          if(Action()=="check_text_input" || Action()=="start_text_input" || Action()=="cancel_text_input") {
            Call(Stage::TextFailureFrameTree,"Page.getFrameTree",nullptr);return;
          }
          Fail("unavailable","The original text request was not acknowledged");return;
        }
        // The document changed under a cached context. Retry once in the new
        // document; a ref from the old one then fails as stale.
        world_=0;world_id_.clear();loader_.clear();world_loader_.clear();world_request_.clear();main_world_=0;
        if(gesture_) { Fail("page_changed","The page changed during the gesture");return; }
        if(pending_->retried) { Fail("page_changed","The page changed while it was being read");return; }
        pending_->retried=true;Begin();return;
      }
      if(At(Stage::FillTyped)) { FillTypedResult(success,result);return; }

      auto remote=result?result->GetDictionary("result"):nullptr;
      auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
      if(!value || value->GetType("ok")!=VTYPE_BOOL) { Fail("script","The page did not return a result");return; }
      if(!value->GetBool("ok")) {
        const auto code=value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"script";
        if(code=="text_input_native_required" && PrivateText() && Action()=="prepare_text_input" &&
            pending_->command->GetBool("native_caret") && !native_attempted_) {
          native_attempted_=true;
          auto params=CefDictionaryValue::Create();
          params->SetString("expression","globalThis.__tiloPage.nativeTextInputElement()");
          params->SetString("objectGroup","tilo-private-native-caret");
          params->SetInt("contextId",text_frame_.world);params->SetBool("silent",true);
          Call(Stage::NativeElement,"Runtime.evaluate",params);return;
        }
        // What the page's own document does not hold may be in a frame it
        // shows: a find that found nothing looks on there.
        if(code=="not_found" && !PrivateText() && Action()=="find" && !frame_doc_ &&
           value->GetType("frames")==VTYPE_LIST && value->GetList("frames")->GetSize()) { FindOn(value,false);return; }
        Fail(code.size()<=64?code:"script",Text(value,"message"));return;
      }
      Act(value);
    }
  }
  // Resolve only node IDs and an editor object in the existing private world.
  // No field contents, attributes or caret offsets are exported to the client; no
  // remote-debugging port or universal-access world is opened.
  void NativeResult(bool success,CefRefPtr<CefDictionaryValue> result) {
    if(Action()=="fill_files") { FileResult(success,result);return; }
    if(!success || !result) { Fail("not_editable","The original native caret cannot be observed");return; }
    auto params=CefDictionaryValue::Create();
    if(At(Stage::NativeElement)) {
      auto remote=result->GetDictionary("result");
      if(!remote || remote->GetType("objectId")!=VTYPE_STRING) { Fail("not_editable","The original native field was lost");return; }
      native_object_=remote->GetString("objectId").ToString();
      Call(Stage::NativeDOM,"DOM.enable",nullptr);
    } else if(At(Stage::NativeDOM)) {
      params->SetInt("depth",0);params->SetBool("pierce",false);
      Call(Stage::NativeDocument,"DOM.getDocument",params);
    } else if(At(Stage::NativeDocument)) {
      params->SetString("objectId",native_object_);
      Call(Stage::NativeNode,"DOM.requestNode",params);
    } else if(At(Stage::NativeNode)) {
      if(result->GetType("nodeId")!=VTYPE_INT || result->GetInt("nodeId")<=0) { Fail("not_editable","The original native field was lost");return; }
      params->SetInt("nodeId",result->GetInt("nodeId"));params->SetBool("pierce",true);
      auto styles=CefListValue::Create();auto style=CefDictionaryValue::Create();
      style->SetString("name","-webkit-user-modify");style->SetString("value","read-write-plaintext-only");
      styles->SetDictionary(0,style);params->SetList("computedStyles",styles);
      Call(Stage::NativeEditors,"DOM.getNodesForSubtreeByStyle",params);
    } else if(At(Stage::NativeEditors)) {
      auto nodes=result->GetList("nodeIds");
      if(!nodes || nodes->GetSize()!=1 || nodes->GetType(0)!=VTYPE_INT) { Fail("not_editable","The native editor cannot be uniquely observed");return; }
      params->SetInt("nodeId",nodes->GetInt(0));params->SetInt("executionContextId",text_frame_.world);
      params->SetString("objectGroup","tilo-private-native-caret");
      Call(Stage::NativeResolve,"DOM.resolveNode",params);
    } else if(At(Stage::NativeResolve)) {
      auto object=result->GetDictionary("object");
      if(!object || object->GetType("objectId")!=VTYPE_STRING) { Fail("not_editable","The original native editor was lost");return; }
      params->SetString("objectId",object->GetString("objectId"));
      params->SetString("functionDeclaration","function(){return globalThis.__tiloPage.configureNativeTextEditor(this)}");
      params->SetBool("returnByValue",true);params->SetBool("silent",true);
      Call(Stage::NativeConfigure,"Runtime.callFunctionOn",params);
    } else {
      auto remote=result->GetDictionary("result");
      if(!remote || remote->GetType("value")!=VTYPE_BOOL || !remote->GetBool("value")) { Fail("text_input_changed","The original native field changed");return; }
      Evaluate();
    }
  }
  // The file input's files, in two steps: the isolated world's object for the
  // element (already validated by `fillFilesTarget`), then the files
  // themselves. `DOM.setFileInputFiles` is the browser's own
  // Enter-on-a-chooser path: the page receives trusted input and change
  // events, and scripts it runs see the files a person would have chosen. The
  // element is its object, never a node id: a node id belongs to the
  // inspected document, and no agent is attached for this world's document.
  void FileResult(bool success,CefRefPtr<CefDictionaryValue> result) {
    auto params=CefDictionaryValue::Create();
    if(At(Stage::FileTarget)) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      // The element arrives as the object itself: an object is sent with its
      // own properties, so a wrapper would arrive without the element in it.
      if(!remote || remote->GetType("objectId")!=VTYPE_STRING) { Fail("page_changed","The file input is no longer on the page");return; }
      file_object_=remote->GetString("objectId").ToString();
      params->SetString("objectId",file_object_);
      params->SetList("files",pending_->command->GetList("paths")->Copy());
      CallIn(Stage::FileSet,world_session_,"DOM.setFileInputFiles",params);
    } else {
      if(!success) { Fail("not_set","The page refused the chosen files");return; }
      auto data=CefDictionaryValue::Create();data->SetList("files",FileNames());data->SetString("mode","dom");
      Succeed(pending_->message,std::nullopt,data);return;
    }
  }
  // Both ends of the gesture, in the view's own space, as the page resolved
  // them: the element's visible centre, or the point the caller gave.
  bool GesturePoints(CefRefPtr<CefDictionaryValue> value,CefPoint& from,CefPoint& to) const {
    const auto read=[&](const char* x,const char* y,CefPoint& point) {
      if(value->GetType(x)!=VTYPE_DOUBLE && value->GetType(x)!=VTYPE_INT) return false;
      // A point of a frame's document is moved by where the frame sits.
      point=CefPoint(static_cast<int>(std::lround(Number(value,x)+world_frame_.x)),static_cast<int>(std::lround(Number(value,y)+world_frame_.y)));
      return true;
    };
    return read("x","y",from) && (Action()!="drag" || read("to_x","to_y",to));
  }
  void GestureLanded(CefRefPtr<CefDictionaryValue> value) {
    // A drawing path resolves its points against the element's box before the
    // pointer moves at all, so it answers with the path and not two ends.
    if(Action()=="path") {
      // A contact path names an element and points, never two ends.
      if(!gesture_page(value)) {Fail("script","The page did not say where the gesture goes");return; }
      GesturePage(Stage::Path,PageGesture::Path);return;
    }
    CefPoint from(0,0),to(0,0);
    if(!GesturePoints(value,from,to)) { Fail("script","The page did not say where the gesture goes");return; }
    pending_->from_x=from.x;pending_->from_y=from.y;pending_->to_x=to.x;pending_->to_y=to.y;
    if(Action()=="hover") {
      // Rest the pointer on the element, then let a frame pass so the page's
      // hover styles and handlers have run before the page is read again.
      MovePointer(from,false);
      CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
        if(self->pending_ && self->pending_->token==token) self->GesturePage(Stage::GestureEnd,PageGesture::End);
      },CefRefPtr<PageAutomation>(this),pending_->token),kHoverFrameMs);
      return;
    }
    PressPointer(from,true);
    // The page's own two ends for a drag it replays itself, and the element
    // it left, so that replay starts from the element the agent named.
    pending_->drag_fallback=value->Copy(true);
    pending_->step=0;
    // The press starts the page's own drag, which the engine opens a turn later
    // (`StartDragging`). The moves have to wait for it, or the release goes out
    // before the drag exists and the page is left holding one nobody ends.
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->DragStep(); },CefRefPtr<PageAutomation>(this)),kPathPointMs);
  }
  // One move of a drag, evenly spaced between its ends, then the release.
  void DragStep() {
    if(!pending_) return;
    const int steps=std::max(1,pending_->steps);
    if(pending_->step<steps) {
      ++pending_->step;
      const double at=static_cast<double>(pending_->step)/steps;
      const CefPoint point(
        static_cast<int>(std::lround(pending_->from_x+(pending_->to_x-pending_->from_x)*at)),
        static_cast<int>(std::lround(pending_->from_y+(pending_->to_y-pending_->from_y)*at)));
      MovePointer(point,true);
      CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->DragStep(); },CefRefPtr<PageAutomation>(this)));
      return;
    }
    const CefPoint end(pending_->to_x,pending_->to_y);
    MovePointer(end,true);
    // The person's button comes up where the drop happened, so the page sees
    // the release it would see from a hand.
    PressPointer(end,false);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(!self->pending_ || self->pending_->token!=token) return;
      // Where the source is still in place nothing took the drop, so the page's
      // own drag and drop handlers are replayed at the same two points.
      self->DragFallback();
    },CefRefPtr<PageAutomation>(this),pending_->token),kReleaseFrameMs);
  }
  // The drop the page replayed itself, over the same two ends, so its own
  // dragstart, dragover and drop handlers move what a person's drag would.
  void DragFallback() {
    auto command=CefDictionaryValue::Create();
    command->SetInt("doc",document_);command->SetString("action","drag_fallback");
    if(!world_frame_.number.empty()) command->SetString("frame",world_frame_.number);
    // The element the drag left, and the one it was dropped on, resolved by the
    // page itself when the gesture began.
    const auto& begin=pending_->drag_fallback;
    const auto ref=[&](const char* key,const char* fallback) {
      if(begin->GetType(key)==VTYPE_STRING) return begin->GetString(key).ToString();
      return begin->GetType(fallback)==VTYPE_STRING?begin->GetString(fallback).ToString():std::string();
    };
    const auto source=ref("from","element"),target=ref("to","to_target");
    if(!source.empty()) command->SetString("element",source);
    if(!target.empty()) command->SetString("to",target);
    // The ends the engine's pointer actually used, in the view's space.
    const double zoom=std::pow(1.2,browser_->GetHost()->GetZoomLevel());
    // The page's script reads points in its own document: a frame's are
    // the pointer's, less where the frame sits.
    command->SetDouble("x",pending_->from_x/zoom-world_frame_.x);command->SetDouble("y",pending_->from_y/zoom-world_frame_.y);
    command->SetDouble("to_x",pending_->to_x/zoom-world_frame_.x);command->SetDouble("to_y",pending_->to_y/zoom-world_frame_.y);
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    CallIn(Stage::GestureEnd,world_session_,"Runtime.evaluate",params);
  }
  // A page drag nothing is driving is refused at the press it would start from
  // (`host.cc`'s `StartDragging`), so a move that arrives without a gesture is
  // ignored.
  void DragMoved() {
    if(!pending_ || (Action()!="drag" && Action()!="path")) return;
    DragStep();
  }
  // A drawing path: a press at the first point, a move through each of the
  // rest, and a release at the last, all in page pixels the page resolved
  // against the element's own box.
  void StartPath(CefRefPtr<CefDictionaryValue> value) {
    std::vector<std::pair<double,double>> path;
    if(!ReadPoints(value->GetList("points"),path)) { Fail("script","The page resolved no drawing path");return; }
    // The points of a frame's document are moved by where the frame sits.
    for(auto& point:path) { point.first+=world_frame_.x;point.second+=world_frame_.y; }
    pending_->points=std::move(path);pending_->point_index=0;pending_->drawing=true;
    pending_->moves=Number(value,"heard");
    const auto first=pending_->points.front();
    PressPointer(CefPoint(static_cast<int>(std::lround(first.first)),static_cast<int>(std::lround(first.second))),true);
    PathHeard(first,kPathPressMs,[this] { PathStep(); });
  }
  // The next point of a path is sent once the page has heard the one
  // before. Moves of the pointer the page has not handled yet reach it as
  // one, so points sent by the clock alone run together on a page that is
  // busy, and a line through seven points has six. The page says when the
  // move arrived; one it never hears (the pointer was there already, or
  // the page is not answering) is waited for briefly and gone on from.
  void PathHeard(std::pair<double,double> point,int64_t wait,std::function<void()> then) {
    const int asked=++pending_->asked_point;const uint64_t token=pending_->token;
    const std::function<void()> heard=[this,asked,token,then] {
      if(!pending_ || pending_->token!=token || pending_->heard_point>=asked) return;
      pending_->heard_point=asked;then();
    };
    auto command=CefDictionaryValue::Create();
    command->SetString("action","pointer_heard");command->SetInt("doc",document_);
    if(!world_frame_.number.empty()) command->SetString("frame",world_frame_.number);
    command->SetDouble("x",std::lround(point.first)-world_frame_.x);command->SetDouble("y",std::lround(point.second)-world_frame_.y);
    command->SetDouble("after",pending_->moves);command->SetInt("wait_ms",static_cast<int>(wait));
    pending_->heard_last=false;
    Ask(world_session_,"Runtime.evaluate",PointerEvaluate(command,world_),[this,heard,asked](bool success,CefRefPtr<CefDictionaryValue> result) {
      // An answer that comes after the path went on without it says nothing.
      if(pending_->heard_point>=asked) return;
      if(auto value=Said(success,result)) {
        pending_->moves=Number(value,"n");
        pending_->heard_last=value->GetType("heard")==VTYPE_BOOL && value->GetBool("heard");
      }
      heard();
    });
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation>,std::function<void()> heard) { heard(); },
      CefRefPtr<PageAutomation>(this),heard),wait+kPathSlackMs);
  }
  static bool ReadPoints(CefRefPtr<CefListValue> points,std::vector<std::pair<double,double>>& out) {
    if(!points || points->GetSize()<2 || points->GetSize()>1024) return false;
    for(size_t i=0;i<points->GetSize();++i) {
      auto pair=points->GetType(i)==VTYPE_LIST?points->GetList(i):nullptr;
      if(!pair || pair->GetSize()!=2) return false;
      // A whole number arrives as an int; a fractional one as a double.
      const auto coordinate=[&](size_t i) { return pair->GetType(i)==VTYPE_DOUBLE?pair->GetDouble(i):static_cast<double>(pair->GetInt(i)); };
      if((pair->GetType(0)!=VTYPE_DOUBLE && pair->GetType(0)!=VTYPE_INT) ||
         (pair->GetType(1)!=VTYPE_DOUBLE && pair->GetType(1)!=VTYPE_INT)) return false;
      const double x=coordinate(0),y=coordinate(1);
      if(!std::isfinite(x) || !std::isfinite(y)) return false;
      out.emplace_back(x,y);
    }
    return true;
  }
  // One more move of the path, then the release at its last point.
  void PathStep() {
    if(!pending_ || !pending_->drawing) return;
    if(++pending_->point_index<pending_->points.size()) {
      const auto point=pending_->points[pending_->point_index];
      // The point before it not having been heard counts against the page:
      // one that hears none of them is not waited on point after point.
      if(pending_->point_index>1 && !pending_->path_blind) {
        pending_->path_missed=pending_->heard_last?0:pending_->path_missed+1;
        if(pending_->path_missed>=kPathMissed) pending_->path_blind=true;
      }
      const auto before=pending_->points[pending_->point_index-1];
      MovePointer(CefPoint(static_cast<int>(std::lround(point.first)),static_cast<int>(std::lround(point.second))),true);
      // A point where the pointer already is moves nothing, so the page
      // hears no move to wait for; and a page that has not said it heard
      // the points before is sent the rest by the clock, a frame apart.
      const bool same=std::lround(point.first)==std::lround(before.first) && std::lround(point.second)==std::lround(before.second);
      if(same || pending_->path_blind) {
        pending_->heard_last=false;
        CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
          if(self->pending_ && self->pending_->token==token) self->PathStep();
        },CefRefPtr<PageAutomation>(this),pending_->token),kPathPointMs);
        return;
      }
      PathHeard(point,kPathHeardMs,[this] { PathStep(); });
      return;
    }
    const auto& last=pending_->points.back();
    // The button comes up where the pointer is. Once the page has heard the
    // pointer arrive there it is not moved there once more: a move of no
    // length is one stroke more on a page that counts them, and whether it
    // came with the move before it or by itself was a matter of timing.
    PointerAt(CefPoint(static_cast<int>(std::lround(last.first)),static_cast<int>(std::lround(last.second))),"up",false,pending_->heard_last);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->GesturePage(Stage::GestureEnd,PageGesture::End);
    },CefRefPtr<PageAutomation>(this),pending_->token),kReleaseFrameMs);
  }
  // The engine's pointer, at a point in the view's space.
  void MovePointer(CefPoint point,bool down) { PointerAt(point,"move",down); }
  void PressPointer(CefPoint point,bool down) { PointerAt(point,down?"down":"up",down); }
  void PointerAt(CefPoint point,const char* type,bool down,bool still=false) {
    if(!browser_ || !pointer_) return;
    auto d=CefDictionaryValue::Create();
    // The app's pointer ops take physical pixels; the view's space is DIPs.
    // The app's pointer step names what it does as `kind`; `down` says the
    // button stays held through the move that follows a press.
    d->SetString("kind",type);d->SetString("type",type);d->SetBool("down",down);
    d->SetDouble("x",point.x);d->SetDouble("y",point.y);
    // A step where the pointer already is presses or lets go without a move.
    if(still) d->SetBool("still",true);
    pointer_(d);
    // The page's `:hover` styles follow the browser's own pointer, which an
    // isolated world cannot move, so the page is told where it came to rest:
    // over the element the button is held on, or nowhere once it came up.
    // A drag never rests the page's pointer: the engine's own drag is in flight
    // across these steps, and a script asked for while it is never answers, so
    // every later page action in this tab would wait for one that cannot come.
    if(Action()=="drag") return;
    RestPointer(down ? point : CefPoint(-1,-1));
  }
  // Tell the page where the pointer rests. Only a hover or a drag rests it:
  // every other action leaves the page as it was.
  void RestPointer(CefPoint point) {
    auto command=CefDictionaryValue::Create();
    command->SetString("action","pointer_rest");
    // Only a hover rests the page's pointer: a drag is running in the engine
    // while its steps go out, and asking for a script then never answers.
    if(Action()!="hover") return;
    // One DevTools method is answered at a time, and the gesture's own step is
    // already on its way: resting the pointer now would replace the call whose
    // answer ends the request, and the request would wait for one that never
    // comes. The rest is remembered and told after the gesture has ended.
    if(call_) { rest_point_=point;rest_pending_=true;return; }
    rest_pending_=false;
    const double zoom=std::pow(1.2,browser_->GetHost()->GetZoomLevel());
    if(point.x>=0 && point.y>=0) { command->SetDouble("x",point.x/zoom-world_frame_.x);command->SetDouble("y",point.y/zoom-world_frame_.y); }
    else command->SetBool("nowhere",true);
    if(!world_frame_.number.empty()) command->SetString("frame",world_frame_.number);
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    params->SetBool("awaitPromise",true);
    CallIn(Stage::Pointer,world_session_,"Runtime.evaluate",params);
  }
  void GestureFinished(CefRefPtr<CefDictionaryValue> value) {
    const auto action=Action();
    std::optional<std::string> outline;
    if(const auto text=Text(value,"outline"); !text.empty()) outline=text;
    Succeed(Text(value,"message"),outline,value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr);
  }
  void Evaluate() {
    auto command=CefDictionaryValue::Create();
    const auto action=Action();
    const bool elementless=!pending_->command->HasKey("element");
    command->SetString("action",action=="click"?"locate":action=="press"?(elementless?"key_target":"focus"):action);
    command->SetInt("doc",PrivateText()?text_frame_.document:document_);
    if(!PrivateText()) Mark(command);
    if(!PrivateText() && !world_frame_.number.empty()) command->SetString("frame",world_frame_.number);
    for(const char* key:{"element","text","key","lease","history_scope"}) if(pending_->command->HasKey(key)) command->SetString(key,pending_->command->GetString(key));
    for(const char* key:{"cross_origin","secret","native_caret","enabled"}) if(pending_->command->HasKey(key)) command->SetBool(key,pending_->command->GetBool(key));
    // A find names what to look for, so the words it matches travel with it.
    for(const char* key:{"role","name"}) if(pending_->command->HasKey(key)) command->SetString(key,pending_->command->GetString(key));
    if(pending_->command->HasKey("nth")) command->SetInt("nth",static_cast<int>(pending_->command->GetInt("nth")));
    if(action=="find" && !frame_doc_ && LooksInFrames()) command->SetBool("all",true);
    for(const char* key:{"x","y"}) if(pending_->command->HasKey(key)) command->SetDouble(key,Number(pending_->command,key));
    for(const char* key:{"quiet_ms","timeout_ms"}) if(pending_->command->HasKey(key)) command->SetInt(key,pending_->command->GetInt(key));
    if(pending_->command->HasKey("expires_ms")) command->SetDouble("expires_ms",Number(pending_->command,"expires_ms"));
    if(pending_->command->HasKey("values")) command->SetList("values",pending_->command->GetList("values")->Copy());
    if(pending_->command->HasKey("paths")) command->SetList("paths",pending_->command->GetList("paths")->Copy());
    // The line a read of the outline starts from, when it reads on.
    if(pending_->command->GetType("from")==VTYPE_INT) command->SetInt("from",pending_->command->GetInt("from"));
    // A form's answers travel whole, by the names the form asks them under.
    if(pending_->command->GetType("form")==VTYPE_STRING) command->SetString("form",pending_->command->GetString("form"));
    if(pending_->command->GetType("answers")==VTYPE_DICTIONARY) command->SetDictionary("answers",pending_->command->GetDictionary("answers")->Copy(false));
    if(pending_->command->HasKey("submit")) command->SetValue("submit",pending_->command->GetValue("submit")->Copy());
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",PrivateText()?text_frame_.world:world_);params->SetBool("returnByValue",true);
    // Private focus/check/start/edit does not grant a site transient activation.
    params->SetBool("userGesture",!PrivateText());params->SetBool("silent",true);
    // `settle` answers with a promise; everything else with a value.
    params->SetBool("awaitPromise",true);
    if(pending_->command->GetString("op")=="text_input") params->SetDouble("timeout",5000);
    CallIn(Stage::Evaluate,PrivateText()?std::string():world_session_,"Runtime.evaluate",params);
  }
  // A capture of an element inside a frame is placed in the page by the
  // frame's own place, exactly as a click inside it is.
  void CaptureRegion(CefRefPtr<CefDictionaryValue> value) {
    pending_->navigation="none";
    if(!value || value->GetType("ok")!=VTYPE_BOOL || !value->GetBool("ok")) {
      Fail(value && value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"script",
        value && value->GetType("message")==VTYPE_STRING?value->GetString("message").ToString()
          :"The page gave no area to picture");return;
    }
    auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
    if(!data) { Fail("script","The page gave no area to picture");return; }
    capture_={Number(data,"x"),Number(data,"y"),Number(data,"width"),Number(data,"height")};
    capture_label_=Text(value,"message");
    if(!(capture_.width>0 && capture_.height>0)) { Fail("invalid","The picture's area is empty");return; }
    // An element read in a frame's document has a box in that document: the
    // frame's place in the page is what moves it into the page's coordinates.
    if(frame_doc_) { PlaceDocument(frame_doc_,false,[this](Clicked place) { CapturePlaced(place); });return; }
    Paint();
  }
  // The frame's place is known: a picture of what sits in it moves by it.
  void CapturePlaced(Clicked place) {
    capture_.x+=place.x;capture_.y+=place.y;
    Paint();
  }
  // Ask the engine to paint the area: the page cannot see, answer or refuse it.
  void Paint() {
    auto clip=CefDictionaryValue::Create();
    clip->SetDouble("x",capture_.x);clip->SetDouble("y",capture_.y);
    clip->SetDouble("width",capture_.width);
    // A picture taller than the engine can paint is cut, and the answer says
    // how much of the area it holds.
    clip->SetDouble("height",std::min(capture_.height,kMaxCaptureHeight));
    // A command that names no width asks for the widest picture the engine
    // paints; one that does is bounded by the same limit.
    const int max_width=pending_->command->HasKey("max_width")
      ?std::min(std::max(1,pending_->command->GetInt("max_width")),kMaxCaptureWidth):kMaxCaptureWidth;
    capture_.max_width=max_width;
    // Never upscaled: a picture is worth nothing larger than the page paints.
    clip->SetDouble("scale",std::min(1.0,static_cast<double>(max_width)/capture_.width));
    auto params=CefDictionaryValue::Create();
    params->SetString("format","png");
    params->SetBool("captureBeyondViewport",true);
    params->SetBool("fromSurface",true);
    params->SetDictionary("clip",clip);
    Call(Stage::Paint,"Page.captureScreenshot",params);
  }
  // The engine's picture: base64 PNG, which the browser stores as the action's
  // own output and shows the agent.
  void Painted(bool success,CefRefPtr<CefDictionaryValue> result) {
    if(!success || !result) { Fail("unavailable","The engine could not paint the page");return; }
    const auto data=result->GetType("data")==VTYPE_STRING?result->GetString("data"):CefString();
    if(data.empty()) { Fail("script","The engine painted nothing");return; }
    // The box is the page's, in page pixels; the picture is that box scaled
    // to no wider than the caller asked for (never wider than the page's own
    // size), so its size is the scaled one, as the pixels that came back.
    const double scale=std::min(1.0, static_cast<double>(capture_.max_width)/std::max(1.0,capture_.width));
    const double height=std::min(capture_.height,kMaxCaptureHeight);
    auto answer=CefDictionaryValue::Create();
    answer->SetDouble("x",capture_.x);answer->SetDouble("y",capture_.y);
    answer->SetDouble("width",std::round(capture_.width*scale));
    answer->SetDouble("height",std::round(height*scale));
    answer->SetDouble("scale",scale);
    answer->SetString("format","png");
    answer->SetString("capture",data);
    Succeed(capture_label_,std::nullopt,answer);
  }
  // A gesture on an element of a frame runs in that frame's document: the
  // ref says which document it was read in. Its points are that document's
  // own, moved by where the frame sits in the page for the engine's
  // pointer. Both ends of a drag are elements of the one document.
  void GestureDocument() {
    const auto read_in=[&](const char* key) {
      if(pending_->command->GetType(key)!=VTYPE_STRING) return 0;
      const auto held=docs_.find(RefDocument(pending_->command->GetString(key).ToString()));
      return held!=docs_.end() && held->second.holder?held->first:0;
    };
    const int from=read_in("element"),to=read_in("to");
    if(Action()=="drag" && (from || to) && (from!=to || pending_->command->HasKey("x") || pending_->command->HasKey("to_x"))) {
      Fail("invalid_request","A drag in a frame names both its ends by elements of that frame");return;
    }
    if(const int number=from?from:to) {
      const PageDocument document=docs_[number];
      world_=document.context;document_=document.number;world_session_=document.session;frame_doc_=document.number;
      world_id_=document.frame;world_frame_=PageFrame{};world_frame_.id=document.frame;
      world_frame_.number=std::to_string(document.handle);pending_->frame=document.handle;
    }
    BeginPageGesture();
  }
  // Whether a find or a search was asked to look in the page's frames too.
  bool LooksInFrames() const { return pending_->command->GetType("frames")==VTYPE_BOOL && pending_->command->GetBool("frames"); }
  static std::string Quoted(const std::string& text) {
    auto value=CefValue::Create();value->SetString(text);
    return CefWriteJSON(value,JSON_WRITER_DEFAULT).ToString();
  }
  // The frames a document listed for this side to look on in, by the
  // numbers the tab calls them: as many as a read of the page reads.
  void LookIn(CefRefPtr<CefListValue> frames) {
    for(size_t i=0;frames && i<frames->GetSize() && pending_->look_frames.size()<kShownFrames;++i) {
      auto frame=frames->GetType(i)==VTYPE_DICTIONARY?frames->GetDictionary(i):nullptr;
      const auto ref=Text(frame,"ref");
      if(ref.empty() || RefDocument(ref)!=document_ || !docs_.count(document_)) continue;
      const int handle=HandleOf(ref);
      if(frame->GetType("label")==VTYPE_STRING) frame_labels_[handle]=Clip(frame->GetString("label").ToString(),120);
      pending_->look_frames.push_back(handle);
    }
  }
  std::string FrameSaid(int handle) const {
    const auto label=frame_labels_.find(handle);
    return "frame "+std::to_string(handle)+(label!=frame_labels_.end() && !label->second.empty()?" ("+Quoted(label->second)+")":std::string());
  }
  static bool Answered(CefRefPtr<CefDictionaryValue> said) { return said && said->GetType("ok")==VTYPE_BOOL && said->GetBool("ok"); }
  // Words are searched for in the page's own document, and in the frames
  // the page shows when the caller asks for that or the page's own document
  // does not say them: a frame is its own document, and often the one that
  // holds what was looked for. A match in a frame names its frame, and its
  // line is one to read on from with that frame.
  void SearchOn(CefRefPtr<CefDictionaryValue> value) {
    auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
    pending_->look=value->Copy(false);
    if(data && (LooksInFrames() || Number(data,"total")<1)) LookIn(data->GetType("frames")==VTYPE_LIST?data->GetList("frames"):nullptr);
    SearchNext();
  }
  void SearchNext() {
    if(pending_->look_index>=pending_->look_frames.size()) { SearchAnswer();return; }
    const int handle=pending_->look_frames[pending_->look_index++];
    auto command=CefDictionaryValue::Create();
    command->SetString("action","search");command->SetString("text",pending_->command->GetString("text"));
    AskFrame(handle,command,[this,handle](CefRefPtr<CefDictionaryValue> said) {
      if(Answered(said)) pending_->looked.emplace_back(handle,said->Copy(false));
      SearchNext();
    });
  }
  void SearchAnswer() {
    auto own=pending_->look;
    auto data=Findings(own);
    if(!data) data=CefDictionaryValue::Create();
    const int here=static_cast<int>(Number(data,"total"));
    auto matches=data->GetType("matches")==VTYPE_LIST?data->GetList("matches")->Copy():CefListValue::Create();
    std::string outline=Text(own,"outline"),where;int there=0;
    for(const auto& [handle,said]:pending_->looked) {
      auto theirs=said->GetType("data")==VTYPE_DICTIONARY?said->GetDictionary("data"):nullptr;
      const int total=theirs?static_cast<int>(Number(theirs,"total")):0;
      if(total<1) continue;
      there+=total;
      const auto number=std::to_string(handle);
      auto found=theirs->GetType("matches")==VTYPE_LIST?theirs->GetList("matches"):nullptr;
      for(size_t i=0;found && i<found->GetSize();++i) {
        if(found->GetType(i)!=VTYPE_DICTIONARY) continue;
        auto match=found->GetDictionary(i)->Copy(false);match->SetString("frame",number);
        matches->SetDictionary(matches->GetSize(),match);
      }
      for(const auto& line:Lines(Text(said,"outline"))) { if(!outline.empty()) outline+='\n';outline+="frame "+number+", "+line; }
      where+=(where.empty()?"":", ")+std::to_string(total)+" in "+FrameSaid(handle);
    }
    // No frame says them either: the page's own answer stands.
    if(!there) { Succeed(Text(own,"message"),Text(own,"outline"),data);return; }
    const auto words=Quoted(Clip(pending_->command->GetString("text").ToString(),200));
    const int all=here+there;
    const std::string message=std::to_string(all)+(all==1?" line says ":" lines say ")+words+": "+
      (here?std::to_string(here)+" in the page's own document, ":"none in the page's own document, ")+where+
      "; read on from a line by giving its number as from, and the frame it is in as frame";
    data->SetList("matches",matches);data->SetInt("total",all);data->SetInt("page_total",here);
    Succeed(message,outline,data);
  }
  // A find names one element. It is looked for in the page's own document,
  // and in the frames the page shows when the caller asks for that or the
  // page's own document holds no match. The matches of every document
  // looked in are counted together, the page's first, by the closest way
  // any of them matched; a match in a frame names its frame, and its ref
  // acts there.
  void FindOn(CefRefPtr<CefDictionaryValue> value,bool counted) {
    pending_->look=value->Copy(false);pending_->look_counted=counted;
    auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
    auto frames=counted?(data && data->GetType("frames")==VTYPE_LIST?data->GetList("frames"):nullptr)
      :(value->GetType("frames")==VTYPE_LIST?value->GetList("frames"):nullptr);
    LookIn(frames);
    FindNext();
  }
  CefRefPtr<CefDictionaryValue> FindCommand() const {
    auto command=CefDictionaryValue::Create();command->SetString("action","find");
    for(const char* key:{"role","name"}) if(pending_->command->GetType(key)==VTYPE_STRING) command->SetString(key,pending_->command->GetString(key));
    return command;
  }
  void FindNext() {
    if(pending_->look_index>=pending_->look_frames.size()) { FindAnswer();return; }
    const int handle=pending_->look_frames[pending_->look_index++];
    auto command=FindCommand();command->SetBool("all",true);
    AskFrame(handle,command,[this,handle](CefRefPtr<CefDictionaryValue> said) {
      if(Answered(said)) pending_->looked.emplace_back(handle,said->Copy(false));
      FindNext();
    });
  }
  void FindAnswer() {
    struct Held { int handle=0,step=3,total=0;CefRefPtr<CefListValue> lines; };
    std::vector<Held> held;
    const auto take=[&](int handle,CefRefPtr<CefDictionaryValue> said) {
      auto data=said && said->GetType("data")==VTYPE_DICTIONARY?said->GetDictionary("data"):nullptr;
      if(!data) return;
      Held one;one.handle=handle;one.step=static_cast<int>(Number(data,"step"));one.total=static_cast<int>(Number(data,"total"));
      one.lines=data->GetType("lines")==VTYPE_LIST?data->GetList("lines"):nullptr;
      if(one.total>0 && one.step>=0 && one.step<3) held.push_back(one);
    };
    auto own=pending_->look;
    auto own_data=own->GetType("data")==VTYPE_DICTIONARY?own->GetDictionary("data"):nullptr;
    if(pending_->look_counted) take(0,own);
    for(const auto& [handle,said]:pending_->looked) take(handle,said);
    // No document holds it: what the page's own document said stands.
    if(held.empty()) { Fail("not_found",pending_->look_counted?Text(own_data,"nothing"):Text(own,"message"));return; }
    int best=3;
    for(const auto& one:held) best=std::min(best,one.step);
    std::vector<Held> counted;int sum=0;
    for(const auto& one:held) if(one.step==best) { counted.push_back(one);sum+=one.total; }
    const std::string asked=pending_->look_counted?Text(own_data,"asked"):Text(own,"asked");
    const int nth=pending_->command->GetType("nth")==VTYPE_INT?pending_->command->GetInt("nth"):0;
    if(!nth && sum>1) {
      std::string listed;int number=0;
      for(const auto& one:counted)
        for(size_t i=0;one.lines && i<one.lines->GetSize() && number<12;++i)
          listed+=(listed.empty()?"":"; ")+std::string("(")+std::to_string(++number)+") "+
            (one.handle?"frame "+std::to_string(one.handle)+": ":std::string())+one.lines->GetString(i).ToString();
      Fail("ambiguous",std::to_string(sum)+" elements are "+asked+" on the page and in its frames: "+listed+
        (sum>number?" and "+std::to_string(sum-number)+" more":std::string())+". Name one by its number, nth, or name its frame");
      return;
    }
    if(nth>sum) {
      Fail("not_found","The page and its frames have "+std::to_string(sum)+(sum==1?" element ":" elements ")+asked+
        ", so there is no ("+std::to_string(nth)+"); name one from 1 to "+std::to_string(sum));
      return;
    }
    int before=0;Held pick=counted.front();
    if(nth) for(const auto& one:counted) { if(nth<=before+one.total) { pick=one;break; } before+=one.total; }
    auto command=FindCommand();
    if(nth) command->SetInt("nth",nth-before);
    const std::function<void(CefRefPtr<CefDictionaryValue>)> found=[this,pick,nth,sum](CefRefPtr<CefDictionaryValue> said) {
      if(!said || said->GetType("ok")!=VTYPE_BOOL) { Fail("page_changed","The page changed while its frames were looked through; find the element again");return; }
      if(!said->GetBool("ok")) { Fail(Text(said,"code").empty()?"not_found":Text(said,"code"),Text(said,"message"));return; }
      auto data=said->GetType("data")==VTYPE_DICTIONARY?said->GetDictionary("data")->Copy(false):CefDictionaryValue::Create();
      data->SetInt("nth",nth?nth:1);data->SetInt("matches",sum);
      std::string message="Found "+Text(data,"line");
      if(pick.handle) { data->SetString("frame",std::to_string(pick.handle));message+=" in "+FrameSaid(pick.handle); }
      if(nth) message+=" of "+std::to_string(sum);
      Succeed(message,std::nullopt,data);
    };
    if(pick.handle) { AskFrame(pick.handle,command,found);return; }
    command->SetInt("doc",document_);
    Ask(world_session_,"Runtime.evaluate",PointerEvaluate(command,world_),[found](bool success,CefRefPtr<CefDictionaryValue> result) { found(Said(success,result)); });
  }
  // A point over a frame is a point of that frame's document: the frame is
  // asked what is there, and a frame inside it in turn. The answer names
  // the element, the frame it is in and a ref that acts there, with the
  // point as the caller gave it. A frame that cannot be read is what the
  // page holds at that point.
  void AtFrame(CefRefPtr<CefDictionaryValue> value,int depth) {
    auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
    const auto ref=Text(data,"frame_element");
    auto inside=data && data->GetType("inside")==VTYPE_DICTIONARY?data->GetDictionary("inside"):nullptr;
    if(ref.empty() || !inside || depth>=kKeyFrameDepth || !docs_.count(RefDocument(ref))) { AtAnswer(value);return; }
    auto command=CefDictionaryValue::Create();
    command->SetString("action","at");command->SetBool("inside",true);
    command->SetDouble("x",Number(inside,"x"));command->SetDouble("y",Number(inside,"y"));
    command->SetDouble("page_x",Number(pending_->command,"x"));command->SetDouble("page_y",Number(pending_->command,"y"));
    AskFrame(HandleOf(ref),command,[this,value,depth](CefRefPtr<CefDictionaryValue> said) {
      if(!Answered(said)) { AtAnswer(value);return; }
      AtFrame(said->Copy(false),depth+1);
    });
  }
  void AtAnswer(CefRefPtr<CefDictionaryValue> value) {
    auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data")->Copy(false):nullptr;
    if(data) { data->Remove("frame_element");data->Remove("inside"); }
    Succeed(Text(value,"message"),std::nullopt,data);
  }
  // What a read found, as its caller is given it: without the frames the
  // page's script listed for this side to look on in.
  static CefRefPtr<CefDictionaryValue> Findings(CefRefPtr<CefDictionaryValue> value) {
    if(!value || value->GetType("data")!=VTYPE_DICTIONARY) return nullptr;
    auto data=value->GetDictionary("data")->Copy(false);
    data->Remove("frames");
    return data;
  }
  static std::vector<std::string> Lines(const std::string& text) {
    std::vector<std::string> lines;
    size_t at=0;
    while(!text.empty() && at<=text.size()) {
      const auto end=text.find('\n',at);
      lines.push_back(text.substr(at,end==std::string::npos?std::string::npos:end-at));
      if(end==std::string::npos) break;
      at=end+1;
    }
    return lines;
  }
  // What a read says of the frames it lists: each is given the number the
  // tab calls it by, and the name the page gave it is kept beside the number.
  void NoteFrames(CefRefPtr<CefDictionaryValue> value) {
    auto data=value && value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
    auto frames=data && data->GetType("frames")==VTYPE_LIST?data->GetList("frames"):nullptr;
    for(size_t i=0;frames && i<frames->GetSize();++i) {
      auto frame=frames->GetDictionary(i);
      if(!frame || frame->GetType("ref")!=VTYPE_STRING) continue;
      const int handle=HandleOf(frame->GetString("ref").ToString());
      if(frame->GetType("label")==VTYPE_STRING) frame_labels_[handle]=Clip(frame->GetString("label").ToString(),120);
    }
  }
  // A read of the page's own document shows what its frames hold. A frame is
  // its own document, and often the one that matters: a site puts its cookie
  // question in a frame of another site. Each frame the page shows (as many
  // as one read carries) is read in its own document, and its first lines
  // stand under the line that lists it, with refs that act in that frame.
  // A frame that cannot be read now keeps its one line, which says how to
  // read it: by its number.
  void ReadShownFrames(CefRefPtr<CefDictionaryValue> value) {
    pending_->read_message=Text(value,"message");pending_->read_outline=Text(value,"outline");
    auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
    auto frames=data && data->GetType("frames")==VTYPE_LIST?data->GetList("frames"):nullptr;
    for(size_t i=0;frames && i<frames->GetSize() && pending_->shown.size()<kShownFrames;++i) {
      auto frame=frames->GetDictionary(i);
      if(!frame || frame->GetType("ref")!=VTYPE_STRING || frame->GetType("at")!=VTYPE_INT || !frame->GetBool("shown")) continue;
      Pending::Shown shown;shown.ref=frame->GetString("ref").ToString();shown.at=frame->GetInt("at");
      pending_->shown.push_back(shown);
    }
    ReadNextFrame();
  }
  void ReadNextFrame() {
    if(pending_->shown_index>=pending_->shown.size()) { FramesRead();return; }
    const size_t index=pending_->shown_index++;
    auto command=CefDictionaryValue::Create();
    command->SetString("action","outline");command->SetInt("most",kShownLines);
    AskFrame(HandleOf(pending_->shown[index].ref),command,[this,index](CefRefPtr<CefDictionaryValue> value) {
      if(value && value->GetType("ok")==VTYPE_BOOL && value->GetBool("ok")) {
        NoteFrames(value);
        auto& shown=pending_->shown[index];
        shown.lines=Lines(Text(value,"outline"));
        auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
        shown.more=data && data->GetType("more")==VTYPE_BOOL && data->GetBool("more");
      }
      ReadNextFrame();
    });
  }
  void FramesRead() {
    auto lines=Lines(pending_->read_outline);
    size_t frames=0,read=0;int last=0;
    // From the last frame to the first, so a line keeps its place until the
    // lines of its own frame go in under it.
    for(auto shown=pending_->shown.rbegin();shown!=pending_->shown.rend();++shown) {
      if(shown->lines.empty() || shown->at<0 || static_cast<size_t>(shown->at)>=lines.size()) continue;
      const auto& lists=lines[static_cast<size_t>(shown->at)];
      const auto text=lists.find_first_not_of(' ');
      const std::string indent((text==std::string::npos?0:text)+2,' ');
      const int handle=HandleOf(shown->ref);
      std::vector<std::string> held;
      for(const auto& line:shown->lines) held.push_back(indent+line);
      if(shown->more) held.push_back(indent+"- text \"… frame "+std::to_string(handle)+" goes on: read the rest of it with frame="+std::to_string(handle)+"\"");
      lines.insert(lines.begin()+shown->at+1,held.begin(),held.end());
      ++frames;read+=shown->lines.size();last=handle;
    }
    std::string outline;
    for(size_t i=0;i<lines.size();++i) { if(i) outline+='\n';outline+=lines[i]; }
    auto message=pending_->read_message;
    if(frames) message+=", and "+std::to_string(read)+(read==1?" line of ":" lines of ")+
      (frames==1?"frame "+std::to_string(last):std::to_string(frames)+" frames");
    Succeed(message,outline);
  }
  // A review of the page covers the frames this tab has worked in: a field an
  // actor set in a frame is reviewed with the page's own, by the frame it is
  // in. Each such frame's fields are read in the frame's own document.
  void ReviewFrames(CefRefPtr<CefDictionaryValue> value) {
    pending_->review=value->Copy(false);
    for(const auto& [number,document]:docs_) {
      if(!document.holder || !document.handle || pending_->review_frames.size()>=kReviewFrames) continue;
      if(std::find(pending_->review_frames.begin(),pending_->review_frames.end(),document.handle)==pending_->review_frames.end())
        pending_->review_frames.push_back(document.handle);
    }
    std::sort(pending_->review_frames.begin(),pending_->review_frames.end());
    ReviewNextFrame();
  }
  void ReviewNextFrame() {
    if(pending_->review_index>=pending_->review_frames.size()) {
      auto review=pending_->review;
      Succeed(Text(review,"message"),Text(review,"outline"),review->GetType("data")==VTYPE_DICTIONARY?review->GetDictionary("data"):nullptr);
      return;
    }
    const int handle=pending_->review_frames[pending_->review_index++];
    auto command=CefDictionaryValue::Create();
    command->SetString("action","fields");
    AskFrame(handle,command,[this,handle](CefRefPtr<CefDictionaryValue> value) {
      if(value && value->GetType("ok")==VTYPE_BOOL && value->GetBool("ok")) ReviewFrame(handle,value);
      ReviewNextFrame();
    });
  }
  void ReviewFrame(int handle,CefRefPtr<CefDictionaryValue> value) {
    auto review=pending_->review;
    auto data=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
    auto fields=data && data->GetType("fields")==VTYPE_LIST?data->GetList("fields"):nullptr;
    auto into=review->GetType("data")==VTYPE_DICTIONARY?review->GetDictionary("data"):nullptr;
    auto all=into && into->GetType("fields")==VTYPE_LIST?into->GetList("fields"):nullptr;
    // A frame that holds no field adds nothing to a review.
    if(!fields || !fields->GetSize() || !all) return;
    const auto number=std::to_string(handle);
    for(size_t i=0;i<fields->GetSize();++i) {
      if(fields->GetType(i)!=VTYPE_DICTIONARY) continue;
      auto field=fields->GetDictionary(i)->Copy(false);
      field->SetString("frame",number);
      all->SetDictionary(all->GetSize(),field);
    }
    auto counts=into->GetType("counts")==VTYPE_DICTIONARY?into->GetDictionary("counts"):nullptr;
    auto theirs=data->GetType("counts")==VTYPE_DICTIONARY?data->GetDictionary("counts"):nullptr;
    if(counts && theirs) for(const char* key:{"action","input","page","changed","unset"})
      counts->SetInt(key,counts->GetInt(key)+theirs->GetInt(key));
    const auto label=frame_labels_.find(handle);
    std::string outline=Text(review,"outline");
    if(!outline.empty()) outline+='\n';
    outline+="- iframe"+(label!=frame_labels_.end() && !label->second.empty()?" \""+label->second+"\"":std::string())+" [frame="+number+"]";
    for(const auto& line:Lines(Text(value,"outline"))) outline+="\n  "+line;
    review->SetString("outline",outline);
    review->SetString("message",Text(review,"message")+"; "+Text(value,"message"));
  }
  void Act(CefRefPtr<CefDictionaryValue> value) {
    const auto action=Action();
    // What the page says the action acted on, by the description that still
    // names it once this document's refs are gone: kept for the tab's record.
    if(value && value->GetType("target")==VTYPE_DICTIONARY) pending_->target=value->GetDictionary("target")->Copy(false);
    // Whether that element is a secret by its nature: what was written into
    // one is kept out of the record whatever the caller said of its text.
    if(value && value->GetType("secret")==VTYPE_BOOL && value->GetBool("secret")) pending_->secret=true;
    Adopt(value);
    // A key pressed without an element goes where the page's focus is, and
    // focus inside a frame is that frame's own to say.
    if(action=="press" && value && value->GetType("framed")==VTYPE_STRING && !pending_->armed) {
      pending_->armed=true;pending_->arm=value->Copy(false);
      KeyInFrame(Text(value,"framed"),0);return;
    }
    // The address of the frame document the action ran in, for the record.
    if(value && value->GetType("document_url")==VTYPE_STRING) pending_->frame_url=Clip(value->GetString("document_url").ToString(),300);
    if(action=="scroll") { ScrollTurn(value);return; }
    // A click inside a frame is a pointer event in the top document, so its
    // point is moved by the frame's place in the page: each document on the
    // way out says where it holds the frame of the one before, first bringing
    // it into view and then, once every one of them has, where it stands. The
    // frame is where the click settles: the page's own script hears neither
    // the pointer nor what the frame does with it, so there is no page fence
    // to run. The frame answers the settle on its own step, after the click
    // has reached it.
    if(action=="click" && !PrivateText() && frame_doc_) {
      clicked_={Number(value,"x"),Number(value,"y")};
      // The page around the frame hears of a frame's click as the frame's own
      // navigation, never as the page leaving its document.
      pending_->navigation="frame";
      pending_->message=Text(value,"message");
      PlaceDocument(frame_doc_,false,[this](Clicked) {
        PlaceDocument(frame_doc_,true,[this](Clicked place) { world_frame_.x=place.x;world_frame_.y=place.y;ClickTarget(); });
      });
      return;
    }
    if(PrivateText() && action=="prepare_text_input") {
      auto data=value?value->GetDictionary("data"):nullptr;
      if(!data || data->GetType("lease")!=VTYPE_STRING || data->GetType("multiline")!=VTYPE_BOOL) { Fail("script","The private preparation returned no field policy");return; }
      text_owner_=text_frame_;text_lease_=data->GetString("lease").ToString();
    }
    if(PrivateText() && (action=="cancel_text_input" || action=="commit_text_input" || action=="undo_text_input" || action=="redo_text_input")) { text_owner_.reset();text_lease_.clear(); }
    // An edit runs the page's input handlers.
    if(action=="commit_text_input" || action=="undo_text_input" || action=="redo_text_input") pending_->navigation="possible";
    // A fill may end in a key that submits the form it is in.
    if(action=="fill" || action=="fill_secret") pending_->navigation="possible";
    // A fill is answered by what it left behind, and by the typing when a
    // region has to be written key by key.
    if(action=="fill") { FillResult(value);return; }
    // A read runs none of the page's code, so it cannot have led anywhere.
    if(action=="outline" || action=="changes") {
      pending_->navigation="none";NoteFrames(value);
      // The page's own document is read with what its frames show.
      if(action=="outline" && !frame_doc_) { ReadShownFrames(value);return; }
      Succeed(Text(value,"message"),Text(value,"outline"));return;
    }
    // A find reads the page as either of those does: it runs none of the
    // page's code, so it cannot have led anywhere, and what it found is its
    // answer's findings.
    if(action=="find") { pending_->navigation="none";
      // Asked to look in the page's frames too, the page's own document
      // lists every match for this side to count with theirs.
      auto found=value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr;
      if(found && found->HasKey("step") && !frame_doc_) { FindOn(value,true);return; }
      Succeed(Text(value,"message"),std::nullopt,found);return; }
    if(action=="search" && !frame_doc_) { pending_->navigation="none";SearchOn(value);return; }
    if(action=="at" && !PrivateText()) { AtFrame(value->Copy(false),0);return; }
    // The fields of the page with who set each, and its forms as the
    // questions they ask: reads like the outline.
    if(action=="fields" && !frame_doc_) { pending_->navigation="none";ReviewFrames(value);return; }
    if(action=="fields" || action=="forms" || action=="search") { pending_->navigation="none";
      Succeed(Text(value,"message"),Text(value,"outline"),Findings(value));return; }
    // A form answered whole runs the page's input handlers, and may send
    // the form: it answers once the page has handled that, with what the
    // page said of each answer.
    if(action=="fill_form") {
      pending_->navigation="possible";
      if(value->GetType("data")==VTYPE_DICTIONARY) pending_->result_data=value->GetDictionary("data")->Copy(false);
    }
    if(action=="fill_files") { FileTarget(value);return; }
    // A picture of the page is taken after the page says where the area is:
    // the page's own box, never one the caller made up.
    if(action=="capture") { CaptureRegion(value);return; }
    if(action=="at" || action=="settle" || action=="reader" || action=="prepare_text_input" || action=="start_text_input" || action=="check_text_input" || action=="commit_text_input" || action=="cancel_text_input" || action=="undo_text_input" || action=="redo_text_input") {
      Succeed(Text(value,"message"),std::nullopt,value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr,
        PrivateText() && value->GetType("history")==VTYPE_DICTIONARY?value->GetDictionary("history"):nullptr);return;
    }
    if(action=="click") {
      const auto number=[&](const char* key){return value->GetType(key)==VTYPE_DOUBLE?value->GetDouble(key):value->GetInt(key);};
      clicked_={number("x"),number("y")};
      ClickTarget();
    } else if(action=="press") {
      // A key the engine cannot press is named back, rather than sent as a key
      // nothing types: a press the page ignored and a press of a key that does
      // not exist would otherwise read the same to the caller.
      if(const auto written=pending_->command->GetString("key").ToString();
         !tilo::NamedKeyMapping(written) && written.size()!=1)
        { Fail("invalid_key",NotAKey(written));return; }
      auto key=CefDictionaryValue::Create();key->SetString("key",pending_->command->GetString("key"));
      for(const char* flag:{"ctrl","shift","alt","meta"}) key->SetBool(flag,pending_->command->GetBool(flag));
      key_(key);
    }
    if(action=="press") pending_->message=Pressed();
    else if(action!="click") pending_->message=Text(value,"message");
    else pending_->message="Clicked "+Text(pending_->command,"element");
    // Answers once the page has handled the input: see `fence` in page_automation.js.
    auto params=CefDictionaryValue::Create();
    params->SetString("expression","globalThis.__tiloPage.fence()");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);
    params->SetBool("awaitPromise",true);params->SetBool("silent",true);
    CallIn(Stage::Fence,world_session_,"Runtime.evaluate",params);
  }
  // What the fill left behind. A field answers as it always has. A region the
  // page kept as it was given is written again as typing (see `TypeFill`); a
  // region whose own editor changed it says what it holds instead, so the
  // fill never claims a text the page is not showing.
  void FillResult(CefRefPtr<CefDictionaryValue> value) {
    if(value->GetType("input")!=VTYPE_LIST) { Succeed(Text(value,"message"),std::nullopt);return; }
    if(value->GetType("kept")==VTYPE_BOOL && value->GetBool("kept")) { TypeFill(value);return; }
    const auto shown=value->GetType("shown")==VTYPE_STRING?value->GetString("shown").ToString():std::string();
    const auto asked=pending_->command->GetString("text").ToString();
    if(shown==asked) { Fail("rejected",Text(pending_->command,"element")+" did not take the text");return; }
    Fail("rejected",Text(pending_->command,"element")+" did not take the text; it holds \""+
      CefString(shown.substr(0,200)).ToString()+"\"");
  }
  // A fill into a contenteditable region is written as a person writes it:
  // the region was emptied, and its lines are typed through the browser's own
  // key input. Only keys ask the page first, with beforeinput, so an editor
  // that keeps its own model (ProseMirror, Quill and the like) hears the edit
  // it would hear from the keyboard and writes the text itself.
  void TypeFill(CefRefPtr<CefDictionaryValue> value) {
    const auto runs=value->GetList("input");
    if(!runs || runs->GetSize()==0 || runs->GetSize()>kMaxFillRuns) {
      Fail("rejected","The field did not keep the edited text");return;
    }
    for(size_t run=0;run<runs->GetSize();++run)
      if(runs->GetType(run)!=VTYPE_STRING) { Fail("rejected","The field did not keep the edited text");return; }
    // The region is empty and focused, its caret where its text was: what the
    // keys write is what it holds afterwards.
    auto region=CefDictionaryValue::Create();
    region->SetList("lines",runs->Copy());
    region->SetInt("run",0);region->SetInt("offset",0);
    pending_->command->SetDictionary("typed",region);
    // The edit that emptied the region has not settled in the page yet, so the
    // first key waits a turn, as a person's first key follows their selection.
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) {
      if(self->pending_) self->TypeStep();
    },CefRefPtr<PageAutomation>(this)),kTypeStartMs);
  }
  // One step: a key at a time while the run has text left, and then the Enter
  // that starts the next line the way a key press would.
  void TypeStep() {
    auto region=pending_->command->GetDictionary("typed");
    if(!region) { Fail("rejected","The field did not keep the edited text");return; }
    const int run=region->GetInt("run");
    const CefRefPtr<CefListValue> lines=region->GetList("lines");
    if(run>=static_cast<int>(lines->GetSize())) { Typed();return; }
    const std::u16string line=lines->GetString(run).ToString16();
    auto offset=static_cast<size_t>(region->GetInt("offset"));
    // Every line after the first starts the way a person starts it: with Enter.
    if(offset>=line.size()) {
      // The run is typed; the next one starts with Enter, and the last one
      // ends the fill without a line after it.
      if(run+1>=static_cast<int>(lines->GetSize())) { Typed();return; }
      region->SetInt("run",run+1);region->SetInt("offset",0);
      TypeKey(CefDictionaryValue::Create(),"",true);return;
    }
    const char16_t unit=line[offset];
    const bool pair=unit>=0xd800 && unit<=0xdbff && offset+1<line.size() && line[offset+1]>=0xdc00 && line[offset+1]<=0xdfff;
    const size_t width=pair?2:1;
    region->SetInt("offset",static_cast<int>(offset+width));
    TypeKey(CefDictionaryValue::Create(),CefString(line.substr(offset,width)).ToString(),false);
  }
  // One key of that typing, sent from the next turn of the loop: a key sent
  // while a DevTools call of this step is answering is not one the page can
  // count on, and the page handles each key before the next is sent.
  void TypeKey(CefRefPtr<CefDictionaryValue> key,const std::string& text,bool enter) {
    key->SetBool("typed",true);key->SetString("text",text);key->SetBool("enter",enter);
    for(const char* flag:{"ctrl","shift","alt","meta"}) key->SetBool(flag,false);
    key_(key);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) {
      if(self->pending_) self->TypeStep();
    },CefRefPtr<PageAutomation>(this)),kTypeKeyMs);
  }
  void Typed() {
    auto params=CefDictionaryValue::Create();
    auto region=CefValue::Create();region->SetDictionary(pending_->command->GetDictionary("typed"));
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.typed("+
      CefWriteJSON(region,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("silent",true);
    CallIn(Stage::FillTyped,world_session_,"Runtime.evaluate",params);
  }
  void FillTypedResult(bool success,CefRefPtr<CefDictionaryValue> result) {
    auto remote=success && result?result->GetDictionary("result"):nullptr;
    auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
    if(!value || value->GetType("ok")!=VTYPE_BOOL || !value->GetBool("ok")) {
      Fail(value && value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"rejected",
        value && value->GetType("message")==VTYPE_STRING?value->GetString("message").ToString()
          :"The field did not keep the edited text");return;
    }
    Succeed(Text(value,"message"),std::nullopt);
  }
  // The page's code has run and the fill into its region has finished: what
  // the page did with it, if anything, is what the caller is told.
  void Fenced(const std::string& navigation) {
    pending_->navigation=navigation=="started"?"started":"possible";
    // What an action did in a frame it did to the frame: a form sent there
    // takes the frame to another document, not the page. The frame's own
    // document says when that has happened.
    if(frame_doc_) {
      pending_->navigation="frame";
      if(file_dialog_)
        pending_->message+="; the page opened a file chooser, which is answered by fill-page-files for that element";
      if(navigation!="started") { Succeed(pending_->message,std::nullopt);return; }
      SettleFrame();return;
    }
    // A click that opened the page's file chooser is held, not shown: say so,
    // so the caller knows the page is waiting and how to answer it.
    if(file_dialog_)
      pending_->message+="; the page opened a file chooser, which is answered by fill-page-files for that element";
    if(pending_->navigation!="started") { Succeed(pending_->message,std::nullopt);return; }
    if(navigations_==pending_->navigations) pending_->awaiting_start=true;
    else if(browser_->IsLoading()) pending_->awaiting_load=true;
    else { Succeed(pending_->message,std::nullopt);return; }
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->Succeed(self->pending_->message+
      "; the page did not finish loading that navigation in time",std::nullopt);
    },CefRefPtr<PageAutomation>(this),pending_->token),kLoadWaitMs);
  }
  // The click, at the point the page resolved: a point of a frame's document
  // moved by where the frame sits in the page. In the page's own document
  // the page's fence says what the click did; a frame is where its own click
  // settles, so the frame is asked for its settled state once the click and
  // this turn have run.
  void ClickTarget() {
    Click click=click_;
    click(clicked_.x+world_frame_.x,clicked_.y+world_frame_.y);
    if(!frame_doc_) { pending_->navigation="possible";return; }
    pending_->navigation="frame";
    SettleFrame();
  }
  void SettleFrame() {
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run({action:'settle',quiet_ms:250,timeout_ms:5000})");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("awaitPromise",true);params->SetBool("silent",true);
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,CefRefPtr<CefDictionaryValue> params,std::string session,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->CallIn(Stage::FrameSettle,session,"Runtime.evaluate",params);
    },CefRefPtr<PageAutomation>(this),params,world_session_,pending_->token));
  }
  // A frame's settle answers from the frame's own document. What the action
  // did may have replaced that document or taken the frame off the page (a
  // consent dialog answered is gone): no answer from it is the frame moving
  // on, which is what the action did, never the page leaving.
  void FrameSettled(bool success,const std::string& json) {
    if(!pending_) return;
    std::string said;
    if(success) {
      auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
      auto answer=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
      auto remote=answer?answer->GetDictionary("result"):nullptr;
      auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
      said=Text(value,"message");
    }
    if(pending_->message.empty()) pending_->message=said;
    // A click that opened the frame's file chooser is held, not shown, as
    // in the page's own document: the answer says so.
    if(file_dialog_ && pending_->message.find("file chooser")==std::string::npos)
      pending_->message+="; the page opened a file chooser, which is answered by fill-page-files for that element";
    // What was done in the frame made the page itself leave: the answer
    // waits for that load, as any other action's does.
    if(navigations_!=pending_->navigations && browser_->IsLoading()) {
      pending_->awaiting_load=true;
      CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
        if(self->pending_ && self->pending_->token==token) self->Succeed(self->pending_->message+
          "; the page did not finish loading that navigation in time",std::nullopt);
      },CefRefPtr<PageAutomation>(this),pending_->token),kLoadWaitMs);
      return;
    }
    Succeed(pending_->message,std::nullopt);
  }
  // What a key the engine cannot press is answered with: the key it was
  // given, the keys it could have meant, and the case the caller need not match.
  static std::string NotAKey(const std::string& written) {
    std::string names;
    for(const auto& named:tilo::kNamedKeys) names+=(names.empty()?"":", ")+std::string(named.name);
    return "\""+written+"\" is not a key the browser has. Name one of "+names+
      " (in any letter case), or a single character such as an a";
  }
  std::string Chord() const {
    static constexpr std::pair<const char*,const char*> kModifiers[]={{"ctrl","Control+"},{"shift","Shift+"},{"alt","Alt+"},{"meta","Meta+"}};
    std::string chord;const auto& command=pending_->command;
    for(const auto& [flag,name]:kModifiers) if(command->GetBool(flag)) chord+=name;
    const auto key=command->GetString("key").ToString();
    return chord+(key==" "?"Space":key);
  }
  static std::string Text(CefRefPtr<CefDictionaryValue> value,const char* key) {
    return value && value->GetType(key)==VTYPE_STRING?value->GetString(key).ToString():std::string();
  }
  // A number the page wrote: the JSON object it arrives in holds it as either
  // a double or an int, and a key it never wrote counts as zero.
  static double Number(CefRefPtr<CefDictionaryValue> d,const char* key) {
    const auto type=d->GetType(key);
    return type==VTYPE_DOUBLE?d->GetDouble(key):type==VTYPE_INT?d->GetInt(key):0;
  }
  void Succeed(const std::string& message,std::optional<std::string> outline,CefRefPtr<CefDictionaryValue> data=nullptr,CefRefPtr<CefDictionaryValue> history=nullptr) {
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",true);
    if(!data && pending_->result_data) data=pending_->result_data;
    fields->SetString("url",browser_->GetMainFrame()->GetURL());fields->SetString("message",message.substr(0,1024));
    // A frame is not a way for the page to leave its document; what was done
    // in one may still have made the page itself leave, which the browser
    // then began.
    const bool in_frame=pending_->navigation=="frame";
    // A navigation the page asked for that the browser never began is not one.
    const bool navigated=(pending_->navigation=="started" || in_frame) && navigations_!=pending_->navigations;
    fields->SetString("navigation",navigated?"started":pending_->navigation=="none"||in_frame?"none":"possible");
    fields->SetBool("navigated",navigated);fields->SetBool("loading",navigated && browser_->IsLoading());
    if(outline) fields->SetString("outline",*outline);else fields->SetNull("outline");
    if(data) fields->SetDictionary("data",data->Copy(false));
    if(history) fields->SetDictionary("history",history->Copy(false));
    Finish(fields);
  }
  static CefRefPtr<CefDictionaryValue> Failure(const std::string& code,const std::string& message) {
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",false);
    fields->SetString("code",code.empty()?"script":code);fields->SetString("message",message.substr(0,1024));
    return fields;
  }
  // The page refused a world: the frame it was asked for has left the page,
  // which is a tree that moved while this action was read in it. The page's
  // frames are read again once and the action runs in the page that is there,
  // so it is never answered from a document the page has replaced.
  void WorldRefused() {
    world_=0;contexts_.clear();world_request_.clear();
    if(pending_->refreshed) { Fail("page_changed","The page's frames moved while this action was read; read the page again");return; }
    pending_->refreshed=true;call_=0;++generation_;step_located_=false;Begin();
  }
  void Fail(const std::string& code,const std::string& message) {
    if(PrivateText() && code=="text_input_retired" && text_owner_ && pending_->command->GetString("lease").ToString()==text_lease_) {
      text_owner_.reset();text_lease_.clear();
    }
    Finish(Failure(code,message));
  }
  // Whether an action operates the page, and so takes a place in the tab's
  // record: reading it, waiting on it and picturing it do not.
  static bool Operates(const std::string& action) {
    for(const char* name:{"click","fill","select","fill_files","press","submit","hover","drag","path","pointer","scroll","reader","fill_form"})
      if(action==name) return true;
    return false;
  }
  // The actor and the record's place the page hears of with an action, so
  // the read baseline is the actor's own and the edits the action causes are
  // set down under its name.
  void Mark(CefRefPtr<CefDictionaryValue> command) const {
    if(!pending_->actor.empty()) command->SetString("actor",pending_->actor);
    if(pending_->seq) command->SetDouble("seq",static_cast<double>(pending_->seq));
  }
  static double NowMs() {
    return std::chrono::duration<double,std::milli>(std::chrono::system_clock::now().time_since_epoch()).count();
  }
  // At most `most` bytes of `text`, never ending inside a character.
  static std::string Clip(std::string text,size_t most) {
    if(text.size()<=most) return text;
    size_t end=most;
    while(end>0 && (static_cast<unsigned char>(text[end])&0xC0)==0x80) --end;
    text.resize(end);return text;
  }
  // The action that is running, for an actor told to wait or turned away.
  std::string Running() const {
    if(!pending_) return "nothing";
    return pending_->actor.empty()?pending_->asked:pending_->asked+" by \""+pending_->actor+"\"";
  }
  std::string Holding() const {
    return "This tab has "+std::to_string(waiting_.size())+" page actions waiting behind the one that is running ("+
      Running()+"); ask again once they have answered";
  }
  // An answer for a request that never ran. The action that is running still
  // has the page, so nothing of it is put back here.
  void Unrun(int request,CefRefPtr<CefDictionaryValue> fields) {
    fields->SetInt("request_id",request);
    emit_("page_result",fields);
  }
  // A request waited as long as one may behind the others: it is answered as
  // never run rather than left to wait on a tab that is not moving.
  void WaitedOut(int request) {
    for(auto it=waiting_.begin();it!=waiting_.end();++it) {
      if((*it)->GetInt("request_id")!=request) continue;
      waiting_.erase(it);
      Unrun(request,Failure("busy","Waited "+std::to_string(kMaxWaitMs/1000)+" seconds behind other page actions in this tab and never ran, "
        "so it changed nothing; running now: "+Running()));
      return;
    }
  }
  // The next action that waited its turn runs, now that the tab is free.
  void RunWaiting() {
    if(pending_ || waiting_.empty() || !browser_) return;
    auto next=waiting_.front();waiting_.pop_front();
    Run(next,true);
  }
  // How far an actor has heard of what the others did in this tab.
  void Seen(const std::string& actor,uint64_t seq) {
    auto& seen=seen_[actor];
    seen=std::max(seen,seq);
    if(seen_.size()>kMaxActors) seen_.erase(seen_.begin()->first==actor?std::next(seen_.begin()):seen_.begin());
  }
  // What a press says of itself, in its answer and in the tab's record. A
  // key that writes its character into a secret field is not named, and nor
  // is one that went where the engine could not read what it was written
  // into: the keys of a password are the password.
  std::string Pressed() const {
    const auto key=pending_->command->GetString("key").ToString();
    const bool writes=key.size()==1 && !tilo::NamedKeyMapping(key);
    if(writes && pending_->secret) return "Pressed a key in a secret field";
    if(writes && pending_->unseen) return "Pressed a key in a frame whose focus could not be read";
    return "Pressed "+Chord();
  }
  // A key pressed without naming an element goes where the page's focus is.
  // Focus inside a frame is that frame's own to say: its document is asked
  // where the key goes, and a frame inside it in turn. The frame hears that
  // the action began, so the key's edit is the action's there too. A frame
  // that cannot be asked leaves the key's place unseen.
  void KeyInFrame(const std::string& ref,int depth) {
    if(ref.empty() || depth>=kKeyFrameDepth || !docs_.count(RefDocument(ref))) { pending_->unseen=true;KeyPlaced();return; }
    const int handle=HandleOf(ref);
    auto command=CefDictionaryValue::Create();command->SetString("action","key_target");
    AskFrame(handle,command,[this,handle,depth](CefRefPtr<CefDictionaryValue> said) {
      if(!said || said->GetType("ok")!=VTYPE_BOOL || !said->GetBool("ok")) { pending_->unseen=true;KeyPlaced();return; }
      pending_->frame=handle;
      if(said->GetType("document_url")==VTYPE_STRING) pending_->frame_url=Clip(said->GetString("document_url").ToString(),300);
      if(said->GetType("framed")==VTYPE_STRING) { KeyInFrame(said->GetString("framed").ToString(),depth+1);return; }
      if(said->GetType("target")==VTYPE_DICTIONARY) pending_->target=said->GetDictionary("target")->Copy(false);
      if(said->GetType("secret")==VTYPE_BOOL && said->GetBool("secret")) pending_->secret=true;
      KeyPlaced();
    });
  }
  void KeyPlaced() { auto arm=pending_->arm;pending_->arm=nullptr;Act(arm); }
  // What the page's own world may call to tell this side something without
  // being asked: `context` names one world, none the world of every
  // document by its name. Only the world this side made holds it; the
  // page's scripts can neither see nor call it.
  static CefRefPtr<CefDictionaryValue> NoteBinding(int context) {
    auto binding=CefDictionaryValue::Create();binding->SetString("name",kNoteBinding);
    if(context) binding->SetInt("executionContextId",context);
    else binding->SetString("executionContextName","tilo-page-actions");
    return binding;
  }
  // The world's note: what a person typed into a field (set down in the
  // tab's record), or that the page changed a field or a form itself (told
  // to the app). A note is the word of a renderer, so it is taken only from
  // a document this side is reading, bounded in size and in number, and
  // nothing of it is kept but what a read of the same field would give.
  void Noted(const std::string& session,CefRefPtr<CefDictionaryValue> params) {
    if(!browser_ || !params || params->GetType("name")!=VTYPE_STRING || params->GetString("name")!=kNoteBinding) return;
    if(params->GetType("executionContextId")!=VTYPE_INT || params->GetType("payload")!=VTYPE_STRING) return;
    const int context=params->GetInt("executionContextId");
    const PageDocument* found=nullptr;
    for(const auto& [number,held]:docs_) if(held.context==context && held.session==session) { found=&held;break; }
    if(!found) return;
    const PageDocument document=*found;
    const auto payload=params->GetString("payload").ToString();
    if(payload.size()>kMaxNoteBytes) return;
    const double now=Now();
    while(!noted_.empty() && now-noted_.front()>kNoteWindowMs) noted_.pop_front();
    if(noted_.size()>=kMaxNotes) return;
    noted_.push_back(now);
    auto parsed=CefParseJSON(payload,JSON_PARSER_RFC);
    auto note=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    if(!note) return;
    const auto kind=Text(note,"kind");
    if(kind=="input") NoteInput(document,note);
    else if(kind=="changed") NoteChanged(document,note);
  }
  static std::string Word(CefRefPtr<CefDictionaryValue> value,const char* key,size_t most) { return Clip(Text(value,key),most); }
  // The frame a note came from, as the record says it of an action there.
  void NoteFrame(const PageDocument& document,CefRefPtr<CefDictionaryValue> note,CefRefPtr<CefDictionaryValue> into) const {
    if(!document.handle) return;
    into->SetString("frame",std::to_string(document.handle));
    if(const auto label=frame_labels_.find(document.handle);label!=frame_labels_.end() && !label->second.empty())
      into->SetString("frame_name",label->second);
    if(const auto address=Word(note,"document_url",300);!address.empty()) into->SetString("frame_url",address);
  }
  // What a person typed straight into the page, one entry to a burst of
  // editing one field: the field as `find` names it, and the text it then
  // holds, or of a secret only that it was set.
  void NoteInput(const PageDocument& document,CefRefPtr<CefDictionaryValue> note) {
    auto entry=CefDictionaryValue::Create();
    entry->SetDouble("time_ms",NowMs());entry->SetString("actor","");entry->SetString("how","input");entry->SetBool("ok",true);
    const auto said=Word(note,"said",kMaxLedgerText);
    entry->SetString("message",said.empty()?"Direct input into the page":"Direct input into "+said);
    entry->SetString("url",browser_->GetMainFrame()->GetURL());
    if(auto target=note->GetType("target")==VTYPE_DICTIONARY?note->GetDictionary("target"):nullptr) {
      auto kept=CefDictionaryValue::Create();
      if(const auto role=Word(target,"role",64);!role.empty()) kept->SetString("role",role);
      if(const auto name=Word(target,"name",kMaxLedgerText);!name.empty()) kept->SetString("name",name);
      if(const double nth=Number(target,"nth");nth>=1 && nth<=100000) kept->SetInt("nth",static_cast<int>(nth));
      if(kept->GetSize()) entry->SetDictionary("target",kept);
    }
    NoteFrame(document,note,entry);
    if(note->GetType("secret")==VTYPE_BOOL && note->GetBool("secret")) entry->SetBool("secret",true);
    else if(note->GetType("text")==VTYPE_STRING) entry->SetString("text",Word(note,"text",kMaxLedgerText));
    SetDownInTurn(entry);
  }
  // What the record says of a hold: `action` names what was held or how it
  // ended, and the message what the person was asked about, by its method
  // and where it would go, and of a download by the file it would save.
  // Nothing of what it carries is known here, so none of it is set down.
  void NoteHold(const std::string& action,const std::string& actor,const std::string& method,const std::string& origin,const std::string& file,const std::string& page={}) {
    if(!browser_) return;
    auto entry=CefDictionaryValue::Create();
    entry->SetDouble("time_ms",NowMs());entry->SetString("actor",Clip(actor,kMaxActorBytes));entry->SetString("how","hold");
    entry->SetString("action",action);entry->SetBool("ok",true);
    std::string said=file.empty()?method:Clip(file,kMaxHeldFile);
    if(!said.empty() && !origin.empty()) said+=file.empty()?" ":", ";
    entry->SetString("message",Clip(said+origin,kMaxLedgerText));
    entry->SetString("url",page.empty()?browser_->GetMainFrame()->GetURL():CefString(page));
    if(!origin.empty()) entry->SetString("to_url",origin);
    SetDownInTurn(entry);
  }
  // An entry no action made takes its place when it is set down: one that
  // comes while an action runs waits for that action's own, which was given
  // its place when it began.
  void SetDownInTurn(CefRefPtr<CefDictionaryValue> entry) {
    if(pending_ && pending_->seq) { if(held_.size()<kMaxHeld) held_.push_back(entry);return; }
    SetDown(entry);
  }
  void SetDown(CefRefPtr<CefDictionaryValue> entry) {
    entry->SetDouble("seq",static_cast<double>(++acted_seq_));
    ledger_.push_back(entry);
    if(ledger_.size()>kMaxLedger) { ledger_.pop_front();++ledger_dropped_; }
    // No action's answer carries this entry, so the app is told of it.
    auto told=CefDictionaryValue::Create();
    told->SetDouble("seq",static_cast<double>(acted_seq_));told->SetDictionary("entry",entry->Copy(false));
    emit_("page_record",told);
  }
  void SetDownHeld() {
    auto held=std::move(held_);held_.clear();
    for(const auto& entry:held) SetDown(entry);
  }
  // The page changed a field or a form itself, or a person typed into one:
  // which document, which forms and which fields, and never what a field
  // holds.
  void NoteChanged(const PageDocument& document,CefRefPtr<CefDictionaryValue> note) {
    auto told=CefDictionaryValue::Create();
    told->SetInt("document",document.number);
    NoteFrame(document,note,told);
    auto forms=CefListValue::Create();
    auto said_forms=note->GetType("forms")==VTYPE_LIST?note->GetList("forms"):nullptr;
    for(size_t i=0;said_forms && i<said_forms->GetSize() && forms->GetSize()<kMaxChangedForms;++i) {
      auto said=said_forms->GetType(i)==VTYPE_DICTIONARY?said_forms->GetDictionary(i):nullptr;
      const auto id=Word(said,"form",64);
      if(id.empty()) continue;
      auto form=CefDictionaryValue::Create();form->SetString("form",id);
      if(const auto signature=Word(said,"signature",16);!signature.empty()) form->SetString("signature",signature);
      if(const auto offers=Word(said,"offers",16);!offers.empty()) form->SetString("offers",offers);
      if(said->GetType("gone")==VTYPE_BOOL && said->GetBool("gone")) form->SetBool("gone",true);
      forms->SetDictionary(forms->GetSize(),form);
    }
    auto fields=CefListValue::Create();
    auto said_fields=note->GetType("fields")==VTYPE_LIST?note->GetList("fields"):nullptr;
    for(size_t i=0;said_fields && i<said_fields->GetSize() && fields->GetSize()<kMaxChangedFields;++i) {
      auto said=said_fields->GetType(i)==VTYPE_DICTIONARY?said_fields->GetDictionary(i):nullptr;
      const auto ref=Word(said,"element",64);const auto how=Text(said,"how");
      // A ref of the document that tells of it, and no other.
      if(RefDocument(ref)!=document.number || (how!="page" && how!="input")) continue;
      auto field=CefDictionaryValue::Create();field->SetString("element",ref);field->SetString("how",how);
      field->SetString("role",Word(said,"role",64));field->SetString("name",Word(said,"name",kMaxLedgerText));
      if(const auto form=Word(said,"form",64);!form.empty()) field->SetString("form",form);
      if(const auto question=Word(said,"question",kMaxLedgerText);!question.empty()) field->SetString("question",question);
      fields->SetDictionary(fields->GetSize(),field);
    }
    if(!forms->GetSize() && !fields->GetSize()) return;
    told->SetList("forms",forms);told->SetList("fields",fields);
    if(const double more=Number(note,"more");more>=1 && more<=100000) told->SetInt("more",static_cast<int>(more));
    emit_("page_changed",told);
  }
  // An operating action is set down in the tab's record as it ends, and
  // every answer to a named actor says what the other actors did in the tab
  // since that actor last heard from it: the page an agent acts on next may
  // not be the page it read, and it is told so rather than left to find out.
  void Record(CefRefPtr<CefDictionaryValue> fields) {
    if(pending_->command->GetString("op")=="text_input" || !pending_->asked_command) return;
    const auto& asked=pending_->asked_command;
    if(pending_->seq) {
      const bool ok=fields->GetType("ok")==VTYPE_BOOL && fields->GetBool("ok");
      auto entry=CefDictionaryValue::Create();
      entry->SetDouble("seq",static_cast<double>(pending_->seq));entry->SetDouble("time_ms",NowMs());
      entry->SetString("actor",pending_->actor);entry->SetString("action",pending_->asked);
      entry->SetBool("ok",ok);
      if(!ok && fields->GetType("code")==VTYPE_STRING) entry->SetString("code",fields->GetString("code"));
      if(fields->GetType("message")==VTYPE_STRING) entry->SetString("message",Clip(fields->GetString("message").ToString(),kMaxLedgerText));
      entry->SetString("url",pending_->began_url);
      const auto now=browser_?browser_->GetMainFrame()->GetURL().ToString():std::string();
      if(!now.empty() && now!=pending_->began_url) entry->SetString("to_url",now);
      if(fields->GetType("navigation")==VTYPE_STRING) entry->SetString("navigation",fields->GetString("navigation"));
      if(pending_->target) entry->SetDictionary("target",pending_->target->Copy(false));
      // An action in a frame is set down with the frame it ran in: the
      // number the tab called it by then, and the name the page gave the
      // frame and the address of its document, which still say which frame
      // it was once the number names nothing.
      if(pending_->frame) {
        entry->SetString("frame",std::to_string(pending_->frame));
        if(const auto label=frame_labels_.find(pending_->frame);label!=frame_labels_.end() && !label->second.empty())
          entry->SetString("frame_name",label->second);
        if(!pending_->frame_url.empty()) entry->SetString("frame_url",pending_->frame_url);
      }
      // What was written is part of the record. A secret never is, and nor
      // is text the page did not take.
      // A secret is one by what the field is, as the page said of the
      // element the action acted on, or because the caller said so.
      const bool secret=pending_->secret || (asked->GetType("secret")==VTYPE_BOOL && asked->GetBool("secret"));
      if(secret) entry->SetBool("secret",true);
      if(ok && !secret && asked->GetType("text")==VTYPE_STRING) entry->SetString("text",Clip(asked->GetString("text").ToString(),kMaxLedgerText));
      if(ok && asked->GetType("values")==VTYPE_LIST) entry->SetList("values",asked->GetList("values")->Copy());
      // A form's answers as the page placed them, by the names it asked.
      if(ok && fields->GetType("data")==VTYPE_DICTIONARY && fields->GetDictionary("data")->GetType("set")==VTYPE_LIST)
        entry->SetList("answers",fields->GetDictionary("data")->GetList("set")->Copy());
      ledger_.push_back(entry);
      if(ledger_.size()>kMaxLedger) { ledger_.pop_front();++ledger_dropped_; }
      fields->SetDouble("seq",static_cast<double>(pending_->seq));
      // What a person typed while the action ran, and what the page was
      // held from sending meanwhile, takes its place after it.
      SetDownHeld();
    }
    if(asked->GetType("actor")!=VTYPE_STRING) return;
    const uint64_t seen=seen_.count(pending_->actor)?seen_[pending_->actor]:0;
    std::vector<CefRefPtr<CefDictionaryValue>> theirs;
    for(const auto& entry:ledger_) {
      if(static_cast<uint64_t>(entry->GetDouble("seq"))<=seen || entry->GetString("actor").ToString()==pending_->actor) continue;
      theirs.push_back(entry);
    }
    Seen(pending_->actor,acted_seq_);
    if(theirs.empty()) return;
    const size_t first=theirs.size()>kMaxOthers?theirs.size()-kMaxOthers:0;
    auto list=CefListValue::Create();
    for(size_t i=first;i<theirs.size();++i) list->SetDictionary(i-first,theirs[i]->Copy(false));
    fields->SetList("others",list);
    if(first) fields->SetInt("others_earlier",static_cast<int>(first));
  }
  void Finish(CefRefPtr<CefDictionaryValue> fields) {
    const int request=pending_->request;const bool text_input=pending_->command->GetString("op")=="text_input";
    // What the page's script wrote of its frames is said by their numbers.
    NumberIn(fields);
    Record(fields);
    SetDownHeld();
    // A rest the gesture could not tell mid-step is told now, while the request
    // that asked for it is still the one running.
    if(rest_pending_) { rest_pending_=false;RestPointer(rest_point_);rest_pending_=false;call_=0; }
    scroll_pending_=false;
    pending_.reset();call_=0;asked_=nullptr;
    if(!waiting_.empty()) { CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->RunWaiting(); },CefRefPtr<PageAutomation>(this))); }
    if(!queued_.empty()) { CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->RunQueued(); },CefRefPtr<PageAutomation>(this))); }
    if(native_attempted_) {
      auto params=CefDictionaryValue::Create();params->SetString("objectGroup","tilo-private-native-caret");
      browser_->GetHost()->ExecuteDevToolsMethod(0,"Runtime.releaseObjectGroup",params);
      native_object_.clear();native_attempted_=false;
    }
    // A chooser the finished action did not answer must not outlive it, or the
    // page would wait for input nobody is going to send.
    if(file_dialog_ && request==file_dialog_request_) CancelFileDialog();
    if(!file_object_.empty()) {
      auto params=CefDictionaryValue::Create();params->SetString("objectGroup","tilo-page-file-input");
      browser_->GetHost()->ExecuteDevToolsMethod(0,"Runtime.releaseObjectGroup",params);
      file_object_.clear();
    }
    Reply(request,fields,text_input);
  }
  // Every answer to an action goes through here, so the page it was run over
  // is put back however the action ended.
  void Reply(int request,CefRefPtr<CefDictionaryValue> fields,bool text_input=false) {
    // A refusal answers for a request that never ran, and the action that is
    // running still needs the page: only the action shows and puts it back.
    if(shown_) { shown_=false;if(action_ended_) action_ended_(); }
    fields->SetInt("request_id",request);
    emit_(text_input?"text_input_result":"page_result",fields);
  }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;
  Emit emit_;Click click_;Key key_;Pointer pointer_;Wheel wheel_;
  // Said around every action: the page is shown for as long as one runs.
  std::function<void()> action_began_,action_ended_;
  // Whether the action that is answering is the one the page was shown for.
  bool shown_=false;
  // The wheel as it turns: how far down the page is left, the page point it
  // turns at, how many turns it has taken and since when. A page with more to
  // load says how much is left after each turn, so the wheel keeps turning
  // until nothing is, within the turns and the time one scroll is allowed.
  struct Scroll { double left=0,x=0,y=0,began=0;int turns=0; } scroll_;
  // A turn of the wheel that is waiting for the world of its own frame.
  bool scroll_pending_=false;
  std::optional<Pending> pending_;
  // The requests the engine refused before they reached this tab, answered
  // where the page's own answers come from; the tab's copy of each runs
  // nothing. A request id is answered once.
  std::set<int> refused_;
  // Requests that came while a refusal or an action was still answering:
  // answered behind it, in the order they came.
  std::vector<CefRefPtr<CefDictionaryValue>> queued_;
  // Actions that came while another ran, waiting their turn in the order
  // they came: as many as may wait, and as long as one may.
  std::deque<CefRefPtr<CefDictionaryValue>> waiting_;
  static constexpr size_t kMaxWaiting=32;
  static constexpr int64_t kMaxWaitMs=30000;
  // The tab's record of the actions that operated it, oldest first, with the
  // place the newest took and how many the record let go of; and how far
  // each actor has heard of the others' part in it.
  std::deque<CefRefPtr<CefDictionaryValue>> ledger_;
  uint64_t acted_seq_=0,ledger_dropped_=0;
  std::map<std::string,uint64_t> seen_;
  static constexpr size_t kMaxLedger=1000,kMaxLedgerRead=200,kMaxOthers=8,kMaxActors=64,kMaxLedgerText=200;
  // The most of an answering actor's name and of a held download's file
  // name the record keeps.
  static constexpr size_t kMaxActorBytes=256,kMaxHeldFile=120;
  // What the page's own world calls to tell this side something (`Noted`),
  // whether the page's inspector has been told of it, and the bounds a
  // renderer's notes are held to: the size of one, how many in a while, how
  // many wait for the action that is running, and how much one tells.
  static constexpr const char* kNoteBinding="__tiloPageNote";
  bool binding_=false;
  static constexpr size_t kMaxNoteBytes=32768,kMaxNotes=40,kMaxHeld=64,kMaxChangedForms=24,kMaxChangedFields=8;
  static constexpr double kNoteWindowMs=2000;
  std::deque<double> noted_;
  std::vector<CefRefPtr<CefDictionaryValue>> held_;
  // How many frames deep a key's focus is followed.
  static constexpr int kKeyFrameDepth=4;
  int stage_=static_cast<int>(Stage::FrameTree);int call_=0;
  // The stage that asked for that call, which its answer is answered in.
  int stage_at_call_=static_cast<int>(Stage::FrameTree);
  std::vector<TextFrame> text_candidates_;TextFrame text_frame_;std::optional<TextFrame> text_owner_;std::string text_lease_;
  bool native_attempted_=false;std::string native_object_;
  std::string file_object_;
  CefRefPtr<CefFileDialogCallback> file_dialog_;int file_dialog_request_=0;
  std::map<int,int> contexts_;uint64_t token_=0,navigations_=0;
  // The documents this tab's actions have run in and still know of, by the
  // number their refs carry: the page's own and its frames'.
  std::map<int,PageDocument> docs_;
  // The page's own document's number, and the frame document the action that
  // is running runs in (none for the page's own), with the connection that
  // document answers on.
  int main_document_=0,frame_doc_=0;std::string world_session_;
  // The numbers the tab calls frames by: each the ref of the element that
  // holds the frame, with the name the page gave it.
  std::map<int,std::string> frame_refs_,frame_labels_;
  std::map<std::string,int> frame_handles_;
  int last_handle_=0;
  // The connections the page's inspector handed out for frames of other
  // sites, by the frame each holds, and whether it was asked to hand them
  // out yet.
  std::map<std::string,std::string> target_sessions_,session_targets_;
  bool auto_attach_=false;
  // The call the action waits on was made on this connection (none for the
  // page's own), and what to do with the answer of a question asked on the
  // way (see `Ask`).
  int next_message_=kFirstSessionCall;std::string call_session_;
  std::function<void(bool,CefRefPtr<CefDictionaryValue>)> asked_;
  // Where the pointer came to rest, when the gesture was mid-step and the page
  // could not be told yet.
  CefPoint rest_point_{-1,-1};bool rest_pending_=false;
  void SetGesture(double x,double y) {gesture_x_=x;gesture_y_=y;}
  int world_=0,document_=0,main_world_=0;std::string main_id_;
  // The document a gesture's world was made in: a cached id is used only for
  // the document it belongs to.
  std::string world_loader_;
  // The frame whose isolated world the pending request is creating, empty for
  // the world the action runs in.
  std::string world_request_;
  CefRefPtr<CefDictionaryValue> tree_;uint64_t tree_generation_=~uint64_t{0};uint64_t generation_=0;
  // The page's own frame and document the tree last read named.
  std::string tree_page_,tree_loader_;
  // The gesture's own step cursor and the last box an element resolved to.
  size_t pointer_index_=0;CefRefPtr<CefDictionaryValue> pointer_box_;
  // Whether the pending action is a pointer gesture, and the place of the
  // frame the gesture currently aims into.
  bool gesture_=false;double gesture_x_=0,gesture_y_=0;
  // What an element step of the pending gesture resolved to: its box when the
  // step was first placed, and the point it is stepped around.
  static constexpr size_t kGestureBoxes=256;
  struct HeldBoxEntry { std::string ref;double x=0,y=0; };
  std::vector<HeldBoxEntry> gesture_boxes_;
  Clicked step_box_;bool step_located_=false;
  // The gesture's frames, in the numbering the page printed.
  std::string loader_,world_id_;PageFrame world_frame_;
  Clicked clicked_;
  // The widest picture a capture paints and the tallest area it covers: a
  // picture is never larger than a person could look at, whatever was asked.
  static constexpr int kMaxCaptureWidth=1600;
  static constexpr double kMaxCaptureHeight=6000;
  // The area a picture covers, in the page's own CSS pixels, and what the
  // page called it.
  struct Area { double x=0,y=0,width=0,height=0; double max_width=kMaxCaptureWidth; };
  Area capture_;
  std::string capture_label_;
  IMPLEMENT_REFCOUNTING(PageAutomation);
};
}
