#pragma once
#include "page_automation_script.h"
#include "include/cef_browser.h"
#include "include/cef_drag_data.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <map>
#include <functional>
#include <chrono>
#include <optional>
#include <string>
#include <utility>
#include <vector>

namespace tilo {
// Documents are numbered across every tab of this host, so a ref read in one
// tab never resolves in another.
inline int page_documents=0;

// Browser-process page reading and operation for the browser's page actions.
// Script runs in an isolated world of the main frame, created through
// DevTools: it shares the page's DOM, but page script can neither see nor
// call it. Clicks and keys go through the same CEF input path as a person's,
// so the page receives trusted events with user activation.
class PageAutomation final : public CefDevToolsMessageObserver {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  // CSS pixels in the main frame's viewport.
  using Click=std::function<void(double,double)>;
  using Key=std::function<void(CefRefPtr<CefDictionaryValue>)>;
  // A pointer move, press (`down` true) or release at a page point.
  using Pointer=std::function<void(CefRefPtr<CefDictionaryValue>)>;
  PageAutomation(CefRefPtr<CefBrowser> browser,Emit emit,Click click,Key key,Pointer pointer)
      :browser_(browser),emit_(std::move(emit)),click_(std::move(click)),key_(std::move(key)),pointer_(std::move(pointer)) {}
  void Attach() { registration_=browser_->GetHost()->AddDevToolsMessageObserver(this); }
  void Detach() { registration_=nullptr;browser_=nullptr;pending_.reset(); }
  // A main-frame navigation began. An action that caused one answers once it
  // loads, so the caller's next read sees the page the action opened.
  void NavigationStarted() { ++navigations_; }
  void NavigationFinished() {
    if(pending_ && pending_->awaiting_load) Succeed(pending_->message,std::nullopt);
  }
  void Request(CefRefPtr<CefDictionaryValue> command) {
    const int request=command->GetInt("request_id");
    if(!browser_) return;
    if(pending_) { Reply(request,Failure("busy","Another page action is still running in this tab"),command->GetString("op")=="text_input");return; }
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
    pending_=Pending{request,++token_,navigations_,false,owned,{}};
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->Fail("timeout","The page did not answer in time");
    },CefRefPtr<PageAutomation>(this),token_),private_text?5000:kTimeoutMs);
    Begin();
  }
  // The page started a drag from the press the pointer just made. Offscreen
  // CEF offers no drag loop, so this host is the drag's target too: it drags
  // over the points the gesture asks for and drops at its end, which the
  // gesture's own step loop drives. False aborts the drag, which is how a page
  // drag nothing is driving ends.
  bool BeginDrag(CefRefPtr<CefDragData> drag_data,CefBrowserHost::DragOperationsMask allowed_ops,int x,int y) {
    if(!browser_ || !pending_ || (Action()!="drag" && Action()!="path")) return false;
    // The whole drag runs on this turn: the source's data is only valid now.
    drag_data_=drag_data->Clone();
    // Whatever the source allows, with a move always among it.
    drag_ops_=static_cast<CefBrowserHost::DragOperationsMask>(static_cast<int>(allowed_ops)|DRAG_OPERATION_MOVE);
    if(drag_ops_==DRAG_OPERATION_NONE) drag_ops_=DRAG_OPERATION_MOVE;
    auto host=browser_->GetHost();
    CefMouseEvent event;event.x=x;event.y=y;event.modifiers=EVENTFLAG_LEFT_MOUSE_BUTTON;
    host->DragTargetDragEnter(drag_data_,event,drag_ops_);
    return true;
  }
  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>,int id,bool success,const void* result,size_t size) override {
    // CEF may answer synchronously, before ExecuteDevToolsMethod returns its id.
    // Copy callback-owned bytes and process on the next UI turn.
    CefRefPtr<PageAutomation> self(this);
    std::string copy(static_cast<const char*>(result),size);
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,int id,bool success,std::string copy) {
      self->Result(id,success,copy);
    },self,id,success,std::move(copy)));
  }
  void OnDevToolsAgentDetached(CefRefPtr<CefBrowser>) override {
    world_=0;contexts_.clear();loader_.clear();text_owner_.reset();text_lease_.clear();
    if(pending_ && call_) Fail("unavailable","The page's inspector connection closed");
  }

 private:
  static constexpr int64_t kTimeoutMs=30000;
  // Long enough for a click's navigation to begin, so its reply can say so.
  static constexpr int64_t kSettleMs=300;
  // A hover's styles have run by the next frame; a release's handlers by
  // shortly after the frame that carried it.
  static constexpr int64_t kHoverFrameMs=80;
  static constexpr int64_t kReleaseFrameMs=80;
  // A drawing path's points are one per frame: pointer moves sent within a
  // single frame are coalesced, and the page would see only the last.
  static constexpr int64_t kPathPointMs=16;
  // A started navigation is awaited this long; a slower one is reported as
  // still loading and the caller waits for it.
  static constexpr int64_t kLoadWaitMs=10000;
  enum class Stage { FrameTree, World, TextWorld, TextFocus, TextFailureFrameTree, NativeElement, NativeDOM, NativeDocument, NativeNode, NativeEditors, NativeResolve, NativeConfigure, Evaluate, GestureBegin, Path, Hover, Pointer, GestureEnd };
  struct TextFrame { std::string id,loader;int world=0,document=0;bool same_origin=true; };
  struct Pending {
    int request;uint64_t token;uint64_t navigations;bool retried;
    CefRefPtr<CefDictionaryValue> command;std::string message;bool awaiting_load=false;
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
    // Whether a native drag started, and whether the page's source moved.
    bool dragged=false;
  };
  // Gestures an agent asks for, which start from the page's own layout.
  bool Dragging() const {
    const auto action=Action();
    return action=="hover" || action=="drag" || action=="path";
  }
  std::string Action() const { return pending_->command->GetString("action").ToString(); }
  bool PrivateText() const { return pending_->command->GetString("op")=="text_input"; }
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
    if(Action()=="press" && !pending_->command->HasKey("element")) { Act(nullptr);return; }
    Call(Stage::FrameTree,"Page.getFrameTree",nullptr);
  }
  // The step of a gesture the page is asked for: what the page holds before
  // the pointer moves, or a point for it to resolve.
  enum class Gesture { Begin, Landing, Path, End };
  // A gesture starts from what the page shows now, so its answer can say
  // what the pointer changed.
  void BeginGesture() {
    pending_->steps=std::max(1,drag_steps());
    // The page's own CSS decides whether a hover has to be mirrored: a page
    // whose hover reveals nothing is driven by the browser's pointer alone.
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run({action:'need_styles'})");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    Call(Stage::Hover,"Runtime.evaluate",params);
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
  void GesturePage(Stage stage,Gesture kind) {
    auto command=CefDictionaryValue::Create();
    const auto& source=pending_->command;
    command->SetInt("doc",document_);
    command->SetBool("gesture_page",true);
    // The page mirrors a hover through its own CSS only when that CSS opens
    // something, which it said before the gesture began.
    command->SetBool("mirror",pending_->hover_styles);
    const std::string action=kind==Gesture::Begin?"gesture_begin":kind==Gesture::End?"gesture_end"
      :kind==Gesture::Path?"path":Action();
    command->SetString("action",action);
    command->SetString("kind",Action());
    for(const char* key:{"element","to"}) if(source->HasKey(key)) command->SetString(key,source->GetString(key));
    for(const char* key:{"x","y","to_x","to_y"}) if(source->HasKey(key)) command->SetDouble(key,Number(source,key));
    // A hover's answer is read where the pointer was rested, which the page
    // only learns once its element has a centre.
    if(kind==Gesture::End && Action()=="hover") {
      const double zoom=std::pow(1.2,browser_->GetHost()->GetZoomLevel());
      command->SetDouble("x",pending_->from_x/zoom);command->SetDouble("y",pending_->from_y/zoom);
    }
    // A drawing path carries its points; the page resolves them from the
    // element's own box, in viewport pixels for the engine's pointer.
    if((kind==Gesture::Begin || kind==Gesture::Path) && source->HasKey("points")) command->SetList("points",source->GetList("points")->Copy());
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    params->SetBool("awaitPromise",true);
    Call(stage,"Runtime.evaluate",params);
  }
  void Call(Stage stage,const char* method,CefRefPtr<CefDictionaryValue> params) {
    stage_=stage;call_=browser_->GetHost()->ExecuteDevToolsMethod(0,method,params);
    if(!call_) Fail("unavailable","The page's inspector connection is unavailable");
  }
  void Result(int id,bool success,const std::string& json) {
    if(!browser_ || !pending_ || !call_ || id!=call_) return;
    call_=0;
    auto parsed=CefParseJSON(json,JSON_PARSER_RFC);
    auto result=parsed && parsed->GetType()==VTYPE_DICTIONARY?parsed->GetDictionary():nullptr;
    // A script context that answers with neither a result nor an exception has
    // gone with its document, which happens when a document is replaced while
    // it is being read. The request is read again in a fresh context, once per
    // request; a page that moved on still fails as changed.
    const bool scripted=stage_==Stage::Evaluate || stage_==Stage::GestureBegin || stage_==Stage::Path ||
      stage_==Stage::Hover || stage_==Stage::Pointer || stage_==Stage::GestureEnd;
    if(scripted && world_ && (!success || !result || (result->GetType("result")!=VTYPE_DICTIONARY && result->GetType("exceptionDetails")!=VTYPE_DICTIONARY))) {
      const auto document=contexts_.count(world_)?contexts_[world_]:0;
      contexts_.erase(world_);world_=0;
      if(document!=document_ && !pending_->retried) { pending_->retried=true;Begin();return; }
    }
    // Never execute a nested DevTools method in its result callback; this
    // already runs on a later UI turn.
    if(stage_==Stage::NativeElement || stage_==Stage::NativeDOM || stage_==Stage::NativeDocument ||
        stage_==Stage::NativeNode || stage_==Stage::NativeEditors || stage_==Stage::NativeResolve || stage_==Stage::NativeConfigure) {
      NativeResult(success,result);return;
    }
    if(stage_==Stage::TextFailureFrameTree) {
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
    if(stage_==Stage::FrameTree) {
      auto tree=success && result?result->GetDictionary("frameTree"):nullptr;
      auto frame=tree?tree->GetDictionary("frame"):nullptr;
      if(!frame || frame->GetType("id")!=VTYPE_STRING || frame->GetType("loaderId")!=VTYPE_STRING) {
        Fail("unavailable","The page has no document to read");return;
      }
      if(PrivateText()) {
        std::vector<TextFrame> frames;
        if(!CollectTextFrames(tree,frames)) { Fail("unavailable","The frame tree cannot be bounded and observed");return; }
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
      const auto loader=frame->GetString("loaderId").ToString();
      // A gesture starts from what the page shows now, through the context it
      // already has; every other request is read through that same context.
      if(world_ && loader==loader_) { if(Dragging()) { BeginGesture();return; } Evaluate();return; }
      loader_=loader;world_=0;
      auto params=CefDictionaryValue::Create();
      params->SetString("frameId",frame->GetString("id"));params->SetString("worldName","tilo-page-actions");
      Call(Stage::World,"Page.createIsolatedWorld",params);
    } else if(stage_==Stage::TextWorld) {
      if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) { ProbeTextFrame();return; }
      text_frame_.world=result->GetInt("executionContextId");
      text_frame_.document=page_documents=page_documents%2147483647+1;
      auto params=CefDictionaryValue::Create();
      // Observe focus only. No transcript or field content is extracted here.
      params->SetString("expression","document.hasFocus() && (()=>{let e=document.activeElement;while(e?.shadowRoot?.activeElement)e=e.shadowRoot.activeElement;return !!e && e.localName!=='iframe' && e.localName!=='frame';})()");
      params->SetInt("contextId",text_frame_.world);params->SetBool("returnByValue",true);params->SetBool("silent",true);
      Call(Stage::TextFocus,"Runtime.evaluate",params);
    } else if(stage_==Stage::TextFocus) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      if(!remote || remote->GetType("value")!=VTYPE_BOOL || !remote->GetBool("value")) { ProbeTextFrame();return; }
      Evaluate();
    } else if(stage_==Stage::World) {
      if(!success || !result || result->GetType("executionContextId")!=VTYPE_INT) {
        Fail("unavailable","The page refused a script context");return;
      }
      world_=result->GetInt("executionContextId");
      // A context belongs to the document it was made in; remember which, so a
      // request read through a context whose document has gone can be read
      // again in a fresh one.
      document_=page_documents=page_documents%2147483647+1;
      contexts_[world_]=document_;
      if(Dragging()) { BeginGesture();return; }
      Evaluate();
    } else if(stage_==Stage::Hover || stage_==Stage::Path || stage_==Stage::GestureBegin || stage_==Stage::GestureEnd) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      auto value=remote?remote->GetDictionary("value"):nullptr;
      if(!value || value->GetType("ok")!=VTYPE_BOOL) { Fail("script","The page did not return a result");return; }
      if(!value->GetBool("ok")) {
        const auto code=value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"script";
        Fail(code.size()<=64?code:std::string("script"),Text(value,"message"));return;
      }
      if(stage_==Stage::Hover) {
        // The stylesheet answer decides whether this page's hover has to be
        // mirrored, and the gesture resolves its ends next.
        pending_->hover_styles=value->GetType("need_styles")==VTYPE_BOOL && value->GetBool("need_styles");
        GesturePage(Stage::GestureBegin,Gesture::Begin);return;
      }
      if(stage_==Stage::GestureBegin) { GestureLanded(value);return; }
      if(stage_==Stage::Path) { StartPath(value);return; }
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
        world_=0;loader_.clear();
        if(pending_->retried) { Fail("page_changed","The page changed while it was being read");return; }
        pending_->retried=true;Begin();return;
      }
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
        Fail(code.size()<=64?code:"script",Text(value,"message"));return;
      }
      Act(value);
    }
  }
  // Resolve only node IDs and an editor object in the existing private world.
  // No field contents, attributes or caret offsets are exported to the client; no
  // remote-debugging port or universal-access world is opened.
  void NativeResult(bool success,CefRefPtr<CefDictionaryValue> result) {
    if(!success || !result) { Fail("not_editable","The original native caret cannot be observed");return; }
    auto params=CefDictionaryValue::Create();
    if(stage_==Stage::NativeElement) {
      auto remote=result->GetDictionary("result");
      if(!remote || remote->GetType("objectId")!=VTYPE_STRING) { Fail("not_editable","The original native field was lost");return; }
      native_object_=remote->GetString("objectId").ToString();
      Call(Stage::NativeDOM,"DOM.enable",nullptr);
    } else if(stage_==Stage::NativeDOM) {
      params->SetInt("depth",0);params->SetBool("pierce",false);
      Call(Stage::NativeDocument,"DOM.getDocument",params);
    } else if(stage_==Stage::NativeDocument) {
      params->SetString("objectId",native_object_);
      Call(Stage::NativeNode,"DOM.requestNode",params);
    } else if(stage_==Stage::NativeNode) {
      if(result->GetType("nodeId")!=VTYPE_INT || result->GetInt("nodeId")<=0) { Fail("not_editable","The original native field was lost");return; }
      params->SetInt("nodeId",result->GetInt("nodeId"));params->SetBool("pierce",true);
      auto styles=CefListValue::Create();auto style=CefDictionaryValue::Create();
      style->SetString("name","-webkit-user-modify");style->SetString("value","read-write-plaintext-only");
      styles->SetDictionary(0,style);params->SetList("computedStyles",styles);
      Call(Stage::NativeEditors,"DOM.getNodesForSubtreeByStyle",params);
    } else if(stage_==Stage::NativeEditors) {
      auto nodes=result->GetList("nodeIds");
      if(!nodes || nodes->GetSize()!=1 || nodes->GetType(0)!=VTYPE_INT) { Fail("not_editable","The native editor cannot be uniquely observed");return; }
      params->SetInt("nodeId",nodes->GetInt(0));params->SetInt("executionContextId",text_frame_.world);
      params->SetString("objectGroup","tilo-private-native-caret");
      Call(Stage::NativeResolve,"DOM.resolveNode",params);
    } else if(stage_==Stage::NativeResolve) {
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
  // Both ends of the gesture, in the view's own space, as the page resolved
  // them: the element's visible centre, or the point the caller gave.
  bool GesturePoints(CefRefPtr<CefDictionaryValue> value,CefPoint& from,CefPoint& to) const {
    const auto read=[&](const char* x,const char* y,CefPoint& point) {
      if(value->GetType(x)!=VTYPE_DOUBLE && value->GetType(x)!=VTYPE_INT) return false;
      point=CefPoint(static_cast<int>(std::lround(Number(value,x))),static_cast<int>(std::lround(Number(value,y))));
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
      GesturePage(Stage::Path,Gesture::Path);return;
    }
    CefPoint from(0,0),to(0,0);
    if(!GesturePoints(value,from,to)) { Fail("script","The page did not say where the gesture goes");return; }
    pending_->from_x=from.x;pending_->from_y=from.y;pending_->to_x=to.x;pending_->to_y=to.y;
    if(Action()=="hover") {
      // Rest the pointer on the element, then let a frame pass so the page's
      // hover styles and handlers have run before the page is read again.
      MovePointer(from,false);
      CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
        if(self->pending_ && self->pending_->token==token) self->GesturePage(Stage::GestureEnd,Gesture::End);
      },CefRefPtr<PageAutomation>(this),pending_->token),kHoverFrameMs);
      return;
    }
    PressPointer(from,true);
    // The page's own two ends for a drag it replays itself, and the element
    // it left, so that replay starts from the element the agent named.
    pending_->drag_fallback=value->Copy(true);
    pending_->step=0;
    CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->DragStep(); },CefRefPtr<PageAutomation>(this)));
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
      if(drag_data_) DragTarget(point);
      CefPostTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->DragStep(); },CefRefPtr<PageAutomation>(this)));
      return;
    }
    const CefPoint end(pending_->to_x,pending_->to_y);
    MovePointer(end,true);
    const bool native=drag_data_!=nullptr;
    if(native) DropDrag(end);
    pending_->dragged=native;
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
    // The native drop may already have moved the source; the page is told so it
    // does not move it a second time.
    command->SetBool("native",pending_->dragged);
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
    command->SetDouble("x",pending_->from_x/zoom);command->SetDouble("y",pending_->from_y/zoom);
    command->SetDouble("to_x",pending_->to_x/zoom);command->SetDouble("to_y",pending_->to_y/zoom);
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    Call(Stage::GestureEnd,"Runtime.evaluate",params);
  }
  // The page's drag, over one more point of its own gesture.
  void DragTarget(CefPoint point) {
    if(!browser_ || !drag_data_) return;
    CefMouseEvent event;event.x=point.x;event.y=point.y;event.modifiers=EVENTFLAG_LEFT_MOUSE_BUTTON;
    browser_->GetHost()->DragTargetDragOver(event,drag_ops_);
  }
  void DropDrag(CefPoint point) {
    if(!browser_ || !drag_data_) return;
    CefMouseEvent event;event.x=point.x;event.y=point.y;event.modifiers=EVENTFLAG_LEFT_MOUSE_BUTTON;
    browser_->GetHost()->DragTargetDrop(event);
    browser_->GetHost()->DragSourceEndedAt(point.x,point.y,DRAG_OPERATION_MOVE);
    browser_->GetHost()->DragSourceSystemDragEnded();
    drag_data_=nullptr;
  }
  // A drawing path: a press at the first point, a move through each of the
  // rest, and a release at the last, all in page pixels the page resolved
  // against the element's own box.
  void StartPath(CefRefPtr<CefDictionaryValue> value) {
    std::vector<std::pair<double,double>> path;
    if(!ReadPoints(value->GetList("points"),path)) { Fail("script","The page resolved no drawing path");return; }
    pending_->points=std::move(path);pending_->point_index=0;pending_->drawing=true;
    const auto& first=pending_->points.front();
    PressPointer(CefPoint(static_cast<int>(std::lround(first.first)),static_cast<int>(std::lround(first.second))),true);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->PathStep(); },CefRefPtr<PageAutomation>(this)),kPathPointMs);
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
      const auto& point=pending_->points[pending_->point_index];
      MovePointer(CefPoint(static_cast<int>(std::lround(point.first)),static_cast<int>(std::lround(point.second))),true);
      CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self) { self->PathStep(); },CefRefPtr<PageAutomation>(this)),kPathPointMs);
      return;
    }
    const auto& last=pending_->points.back();
    PressPointer(CefPoint(static_cast<int>(std::lround(last.first)),static_cast<int>(std::lround(last.second))),false);
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->GesturePage(Stage::GestureEnd,Gesture::End);
    },CefRefPtr<PageAutomation>(this),pending_->token),kReleaseFrameMs);
  }
  // The engine's pointer, at a point in the view's space.
  void MovePointer(CefPoint point,bool down) { PointerAt(point,"move",down); }
  void PressPointer(CefPoint point,bool down) { PointerAt(point,down?"down":"up",down); }
  void PointerAt(CefPoint point,const char* type,bool down) {
    if(!browser_ || !pointer_) return;
    auto d=CefDictionaryValue::Create();
    // The app's pointer ops take physical pixels; the view's space is DIPs.
    d->SetString("type",type);d->SetBool("down",down);
    d->SetDouble("x",point.x);d->SetDouble("y",point.y);
    pointer_(d);
    // The page's `:hover` styles follow the browser's own pointer, which an
    // isolated world cannot move, so the page is told where it came to rest:
    // over the element the button is held on, or nowhere once it came up.
    RestPointer(down ? point : CefPoint(-1,-1));
  }
  // Tell the page where the pointer rests. Only a hover or a drag rests it:
  // every other action leaves the page as it was.
  void RestPointer(CefPoint point) {
    auto command=CefDictionaryValue::Create();
    command->SetString("action","pointer_rest");
    // Only a hover or a drag rests the page's pointer; a path draws with the
    // engine's own contact, and no other action moves the page at all.
    if(Action()!="hover" && Action()!="drag") return;
    const double zoom=std::pow(1.2,browser_->GetHost()->GetZoomLevel());
    if(point.x>=0 && point.y>=0) { command->SetDouble("x",point.x/zoom);command->SetDouble("y",point.y/zoom); }
    else command->SetBool("nowhere",true);
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);params->SetBool("userGesture",false);params->SetBool("silent",true);
    params->SetBool("awaitPromise",true);
    Call(Stage::Pointer,"Runtime.evaluate",params);
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
    command->SetString("action",action=="click"?"locate":action=="press"?"focus":action);
    command->SetInt("doc",PrivateText()?text_frame_.document:document_);
    for(const char* key:{"element","text","key","lease","history_scope"}) if(pending_->command->HasKey(key)) command->SetString(key,pending_->command->GetString(key));
    for(const char* key:{"cross_origin","secret","native_caret","enabled"}) if(pending_->command->HasKey(key)) command->SetBool(key,pending_->command->GetBool(key));
    for(const char* key:{"x","y"}) if(pending_->command->HasKey(key)) command->SetDouble(key,Number(pending_->command,key));
    for(const char* key:{"quiet_ms","timeout_ms"}) if(pending_->command->HasKey(key)) command->SetInt(key,pending_->command->GetInt(key));
    if(pending_->command->HasKey("expires_ms")) command->SetDouble("expires_ms",Number(pending_->command,"expires_ms"));
    if(pending_->command->HasKey("values")) command->SetList("values",pending_->command->GetList("values")->Copy());
    // A gesture names its far end and a path its points; the page resolves both.
    if(pending_->command->HasKey("to")) command->SetString("to",pending_->command->GetString("to"));
    if(pending_->command->HasKey("to_x") && pending_->command->HasKey("to_y")) {
      command->SetDouble("to_x",Number(pending_->command,"to_x"));command->SetDouble("to_y",Number(pending_->command,"to_y"));
    }
    if(pending_->command->HasKey("points")) command->SetList("points",pending_->command->GetList("points")->Copy());
    auto wrapped=CefValue::Create();wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",PrivateText()?text_frame_.world:world_);params->SetBool("returnByValue",true);
    // Private focus/check/start/edit does not grant a site transient activation.
    params->SetBool("userGesture",!PrivateText());params->SetBool("silent",true);
    // `settle` answers with a promise; everything else with a value.
    params->SetBool("awaitPromise",true);
    if(pending_->command->GetString("op")=="text_input") params->SetDouble("timeout",5000);
    Call(Stage::Evaluate,"Runtime.evaluate",params);
  }
  void Act(CefRefPtr<CefDictionaryValue> value) {
    const auto action=Action();
    if(PrivateText() && action=="prepare_text_input") {
      auto data=value?value->GetDictionary("data"):nullptr;
      if(!data || data->GetType("lease")!=VTYPE_STRING || data->GetType("multiline")!=VTYPE_BOOL) { Fail("script","The private preparation returned no field policy");return; }
      text_owner_=text_frame_;text_lease_=data->GetString("lease").ToString();
    }
    if(PrivateText() && (action=="cancel_text_input" || action=="commit_text_input" || action=="undo_text_input" || action=="redo_text_input")) { text_owner_.reset();text_lease_.clear(); }
    if(action=="outline" || action=="changes") { Succeed(Text(value,"message"),Text(value,"outline"));return; }
    if(action=="at" || action=="settle" || action=="reader" || action=="prepare_text_input" || action=="start_text_input" || action=="check_text_input" || action=="commit_text_input" || action=="cancel_text_input" || action=="undo_text_input" || action=="redo_text_input") {
      Succeed(Text(value,"message"),std::nullopt,value->GetType("data")==VTYPE_DICTIONARY?value->GetDictionary("data"):nullptr,
        PrivateText() && value->GetType("history")==VTYPE_DICTIONARY?value->GetDictionary("history"):nullptr);return;
    }
    if(action=="click") {
      const auto number=[&](const char* key){return value->GetType(key)==VTYPE_DOUBLE?value->GetDouble(key):value->GetInt(key);};
      click_(number("x"),number("y"));
    } else if(action=="press") {
      auto key=CefDictionaryValue::Create();key->SetString("key",pending_->command->GetString("key"));
      for(const char* flag:{"ctrl","shift","alt","meta"}) key->SetBool(flag,pending_->command->GetBool(flag));
      key_(key);
    }
    pending_->message=action=="press"?"Pressed "+Chord():Text(value,"message");
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->Settled();
    },CefRefPtr<PageAutomation>(this),pending_->token),kSettleMs);
  }
  void Settled() {
    if(navigations_==pending_->navigations || !browser_->IsLoading()) { Succeed(pending_->message,std::nullopt);return; }
    pending_->awaiting_load=true;
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->Succeed(self->pending_->message,std::nullopt);
    },CefRefPtr<PageAutomation>(this),pending_->token),kLoadWaitMs);
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
  static double Number(CefRefPtr<CefDictionaryValue> d,const char* key) {
    return d->GetType(key)==VTYPE_DOUBLE?d->GetDouble(key):d->GetInt(key);
  }
  void Succeed(const std::string& message,std::optional<std::string> outline,CefRefPtr<CefDictionaryValue> data=nullptr,CefRefPtr<CefDictionaryValue> history=nullptr) {
    auto fields=CefDictionaryValue::Create();fields->SetBool("ok",true);
    fields->SetString("url",browser_->GetMainFrame()->GetURL());fields->SetString("message",message.substr(0,1024));
    const bool navigated=navigations_!=pending_->navigations;
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
  void Fail(const std::string& code,const std::string& message) {
    if(PrivateText() && code=="text_input_retired" && text_owner_ && pending_->command->GetString("lease").ToString()==text_lease_) {
      text_owner_.reset();text_lease_.clear();
    }
    Finish(Failure(code,message));
  }
  void Finish(CefRefPtr<CefDictionaryValue> fields) {
    const int request=pending_->request;const bool text_input=pending_->command->GetString("op")=="text_input";
    pending_.reset();call_=0;
    if(native_attempted_) {
      auto params=CefDictionaryValue::Create();params->SetString("objectGroup","tilo-private-native-caret");
      browser_->GetHost()->ExecuteDevToolsMethod(0,"Runtime.releaseObjectGroup",params);
      native_object_.clear();native_attempted_=false;
    }
    Reply(request,fields,text_input);
  }
  void Reply(int request,CefRefPtr<CefDictionaryValue> fields,bool text_input=false) { fields->SetInt("request_id",request);emit_(text_input?"text_input_result":"page_result",fields); }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;
  Emit emit_;Click click_;Key key_;Pointer pointer_;
  // The drag the page is running, held from its start until its drop.
  CefRefPtr<CefDragData> drag_data_;CefBrowserHost::DragOperationsMask drag_ops_=DRAG_OPERATION_NONE;
  std::optional<Pending> pending_;Stage stage_=Stage::FrameTree;int call_=0;
  std::vector<TextFrame> text_candidates_;TextFrame text_frame_;std::optional<TextFrame> text_owner_;std::string text_lease_;
  bool native_attempted_=false;std::string native_object_;
  int world_=0,document_=0;std::map<int,int> contexts_;std::string loader_;uint64_t token_=0,navigations_=0;
  IMPLEMENT_REFCOUNTING(PageAutomation);
};
}
