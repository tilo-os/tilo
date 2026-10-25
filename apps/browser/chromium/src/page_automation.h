#pragma once
#include "page_automation_script.h"
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_parser.h"
#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"
#include <functional>
#include <chrono>
#include <optional>
#include <string>
#include <utility>
#include <vector>

namespace tilo {
// One frame of the page: its document, the id its parent's outline printed,
// and the hit-testing offset that turns a point in its document into a point
// in the top one.
struct PageFrame { std::string id,loader,address,number;double x=0,y=0; };
// A point in a frame's own document, hit-tested in the top one.
struct Clicked { double x=0,y=0; };

// Documents are numbered across every tab of this host, so a ref read in one
// tab never resolves in another.
inline int page_documents=0;

// Browser-process page reading and operation for the browser's page actions.
// Script runs in an isolated world of one frame — the main frame, or the frame
// an action names with the id its parent's outline printed — created through
// DevTools: it shares that document's DOM, but page script can neither see nor
// call it. Clicks and keys go through the same CEF input path as a person's,
// so the page receives trusted events with user activation; a click inside a
// frame is offset by the frame's place in the page.
class PageAutomation final : public CefDevToolsMessageObserver {
 public:
  using Emit=std::function<void(const std::string&,CefRefPtr<CefDictionaryValue>)>;
  // CSS pixels in the main frame's viewport.
  using Click=std::function<void(double,double)>;
  using Key=std::function<void(CefRefPtr<CefDictionaryValue>)>;
  PageAutomation(CefRefPtr<CefBrowser> browser,Emit emit,Click click,Key key)
      :browser_(browser),emit_(std::move(emit)),click_(std::move(click)),key_(std::move(key)) {}
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
    world_=0;world_id_.clear();loader_.clear();text_owner_.reset();text_lease_.clear();
    frame_world_.reset();world_request_.clear();main_world_=0;
    tree_=nullptr;++generation_;
    if(pending_ && call_) Fail("unavailable","The page's inspector connection closed");
  }

 private:
  static constexpr int64_t kTimeoutMs=30000;
  // Long enough for a click's navigation to begin, so its reply can say so.
  static constexpr int64_t kSettleMs=300;
  // A started navigation is awaited this long; a slower one is reported as
  // still loading and the caller waits for it.
  static constexpr int64_t kLoadWaitMs=10000;
  enum class Stage { FrameTree, World, FrameOffset, TextWorld, TextFocus, TextFailureFrameTree, NativeElement, NativeDOM, NativeDocument, NativeNode, NativeEditors, NativeResolve, NativeConfigure, Evaluate };
  struct TextFrame { std::string id,loader;int world=0,document=0;bool same_origin=true; };
  struct Pending {
    int request;uint64_t token;uint64_t navigations;bool retried;
    CefRefPtr<CefDictionaryValue> command;std::string message;bool awaiting_load=false;
  };
  std::string Action() const { return pending_->command->GetString("action").ToString(); }
  bool PrivateText() const { return pending_->command->GetString("op")=="text_input"; }
  static int FramePart(const std::string& part) {
    if(part.empty() || part.size()>3) return -1;
    int value=0;
    for(const char c:part) { if(c<'0'||c>'9') return -1;value=value*10+(c-'0'); }
    return value>0?value:-1;
  }
  // Frames are numbered from one in document order, both here and in the page
  // script, so `2.1` names the same frame to both.
  static bool ResolveFrame(CefRefPtr<CefDictionaryValue> tree,const std::vector<int>& path,size_t depth,PageFrame& out,const Clicked& inherited={},const std::string& prefix={}) {
    if(!tree || depth>path.size()) return false;
    auto frame=tree->GetDictionary("frame");
    if(!frame || frame->GetType("id")!=VTYPE_STRING || frame->GetType("loaderId")!=VTYPE_STRING) return false;
    if(depth==path.size()) {
      out.id=frame->GetString("id").ToString();out.loader=frame->GetString("loaderId").ToString();
      out.number=prefix;out.x=inherited.x;out.y=inherited.y;
      out.address=frame->GetType("url")==VTYPE_STRING?frame->GetString("url").ToString():std::string();
      return true;
    }
    auto children=tree->GetList("childFrames");
    if(!children || children->GetSize()>4096) return false;
    const auto ordinal=path[depth];
    if(ordinal<1 || static_cast<size_t>(ordinal)>children->GetSize()) return false;
    // Frames of one document are numbered in the order the page script walks
    // them, which is this list's order.
    auto child=children->GetDictionary(static_cast<size_t>(ordinal)-1);
    const auto offset=FrameOffset(child?child->GetDictionary("offset"):nullptr);
    const auto child_prefix=prefix.empty()?std::to_string(ordinal):prefix+"."+std::to_string(ordinal);
    return ResolveFrame(child,path,depth+1,out,{inherited.x+offset.x,inherited.y+offset.y},child_prefix);
  }
  // The frame element's border box inside its parent, as a point in the
  // parent's document: the page script measures it from the shared DOM, never
  // from the frame's own document.
  static Clicked FrameOffset(CefRefPtr<CefDictionaryValue> offset) {
    Clicked out;
    if(!offset) return out;
    for(const char* key:{"x","y"}) {
      const auto type=offset->GetType(key);
      if(type==VTYPE_DOUBLE || type==VTYPE_INT) (key[0]=='x'?out.x:out.y)=Number(offset,key);
    }
    return out;
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
    if(Action()=="press" && !pending_->command->HasKey("element")) { Act(nullptr);return; }
    if(tree_ && tree_generation_==generation_) { AfterTree();return; }
    Call(Stage::FrameTree,"Page.getFrameTree",nullptr);
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
    // The frame the caller named, or the main document. A frame id the
    // page outline never printed is refused rather than read as the main
    // document, so a caller never reads the wrong document believing it
    // read the frame.
    PageFrame target;
    const auto named=pending_->command->GetType("frame")==VTYPE_STRING?pending_->command->GetString("frame").ToString():std::string();
    if(named.empty()) {
      target.id=frame->GetString("id").ToString();target.loader=frame->GetString("loaderId").ToString();
      target.address=frame->GetType("url")==VTYPE_STRING?frame->GetString("url").ToString():std::string();
    } else {
      std::vector<int> path;
      size_t start=0;
      while(start<=named.size() && path.size()<=8) {
        const auto dot=named.find('.',start);
        const auto part=named.substr(start,dot==std::string::npos?std::string::npos:dot-start);
        const int ordinal=FramePart(part);
        if(ordinal<0) { path.clear();break; }
        path.push_back(ordinal);
        if(dot==std::string::npos) break;
        start=dot+1;
      }
      if(path.empty() || path.size()>8) { Fail("no_frame","No frame of this page has that id; read the page again for the frames it lists");return; }
      if(named.size()>16 || !ResolveFrame(tree_,path,0,target)) {
        Fail("no_frame","No frame of this page has that id; read the page again for the frames it lists");return;
      }
    }
    if(world_id_==target.id && world_ && target.loader==loader_) { Evaluate();return; }
    loader_=target.loader;world_=0;world_frame_=target;
    auto params=CefDictionaryValue::Create();
    params->SetString("frameId",target.id);params->SetString("worldName","tilo-page-actions");
    Call(Stage::World,"Page.createIsolatedWorld",params);
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
      main_id_=frame->GetString("id").ToString();
      // CEF owns a result value only for the callback that received it, so the
      // tree is copied before it is kept for the next action.
      tree_=tree->Copy(false);tree_generation_=generation_;
      // The tree leads straight on: which document this action runs in is the
      // next step, never the end of the action.
      AfterTree();return;
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
      // Which world this result is depends on the frame it was asked for, so
      // the page's world and the frame's never stand for each other.
      const int context=result->GetInt("executionContextId");
      const bool asked_for_page=world_request_==main_id_;
      world_request_.clear();
      const bool in_frame=world_frame_.id!=main_id_ && !PrivateText();
      if(in_frame) {
        if(asked_for_page) main_world_=context; else frame_world_=context;
        // A click inside a frame needs the frame's place in the page, which
        // only the page's own world can measure. Asking for it stays a second
        // step: it never retargets the action away from the frame.
        if(!main_world_) {
          world_request_=main_id_;
          auto params=CefDictionaryValue::Create();
          params->SetString("frameId",main_id_);params->SetString("worldName","tilo-page-actions");
          Call(Stage::World,"Page.createIsolatedWorld",params);return;
        }
        if(!frame_world_) { Fail("unavailable","The page refused a script context in this frame");return; }
        world_=*frame_world_;
      } else {
        world_=context;main_world_=context;
      }
      world_id_=world_frame_.id;
      document_=page_documents=page_documents%2147483647+1;
      Evaluate();
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
        world_=0;world_id_.clear();loader_.clear();frame_world_.reset();world_request_.clear();main_world_=0;
        if(pending_->retried) { Fail("page_changed","The page changed while it was being read");return; }
        pending_->retried=true;Begin();return;
      }
      if(stage_==Stage::FrameOffset) { FrameOffsetResult(success,result);return; }
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
  void Evaluate() {
    auto command=CefDictionaryValue::Create();
    const auto action=Action();
    command->SetString("action",action=="click"?"locate":action=="press"?"focus":action);
    command->SetInt("doc",PrivateText()?text_frame_.document:document_);
    if(!PrivateText() && !world_frame_.number.empty()) command->SetString("frame",world_frame_.number);
    for(const char* key:{"element","text","key","lease","history_scope"}) if(pending_->command->HasKey(key)) command->SetString(key,pending_->command->GetString(key));
    for(const char* key:{"cross_origin","secret","native_caret","enabled"}) if(pending_->command->HasKey(key)) command->SetBool(key,pending_->command->GetBool(key));
    for(const char* key:{"x","y"}) if(pending_->command->HasKey(key)) command->SetDouble(key,Number(pending_->command,key));
    for(const char* key:{"quiet_ms","timeout_ms"}) if(pending_->command->HasKey(key)) command->SetInt(key,pending_->command->GetInt(key));
    if(pending_->command->HasKey("expires_ms")) command->SetDouble("expires_ms",Number(pending_->command,"expires_ms"));
    if(pending_->command->HasKey("values")) command->SetList("values",pending_->command->GetList("values")->Copy());
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
  // Ask the main document where the frame now sits: the page script scrolls
  // the frame's element into view before it reads a point in the document.
  void MeasureFrameOffset() {
    auto command=CefDictionaryValue::Create();auto wrapped=CefValue::Create();
    command->SetString("action","frame_offset");command->SetString("frame",world_frame_.number);
    wrapped->SetDictionary(command);
    auto params=CefDictionaryValue::Create();
    params->SetString("expression",std::string(kPageAutomation)+"\n;globalThis.__tiloPage.run("+CefWriteJSON(wrapped,JSON_WRITER_DEFAULT).ToString()+")");
    params->SetInt("contextId",main_world_);params->SetBool("returnByValue",true);params->SetBool("silent",true);
    Call(Stage::FrameOffset,"Runtime.evaluate",params);
  }
  void FrameOffsetResult(bool success,CefRefPtr<CefDictionaryValue> result) {
    auto remote=result?result->GetDictionary("result"):nullptr;
    auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
    if(!success || !value || value->GetType("ok")!=VTYPE_BOOL || !value->GetBool("ok")) {
      Fail(value && value->GetType("code")==VTYPE_STRING?value->GetString("code").ToString():"unavailable",
        value && value->GetType("message")==VTYPE_STRING?value->GetString("message").ToString()
          :"The frame moved off the page before it could be clicked");return;
    }
    const auto number=[&](const char* key){return value->GetType(key)==VTYPE_DOUBLE?value->GetDouble(key):value->GetInt(key);};
    world_frame_.x=number("x");world_frame_.y=number("y");    ClickTarget();
  }
  void ClickTarget() {
    // A click is a pointer event in the top document, so a point read in a
    // frame's own document moves by the frame's place in the page.
    click_(clicked_.x+world_frame_.x,clicked_.y+world_frame_.y);
    pending_->message="Clicked "+Text(pending_->command,"element");
    SettleAction();
  }
  // Every action that changes the page ends the same way: it reports once the
  // page has settled, or when the navigation it started has landed.
  void SettleAction() {
    CefPostDelayedTask(TID_UI,base::BindOnce([](CefRefPtr<PageAutomation> self,uint64_t token) {
      if(self->pending_ && self->pending_->token==token) self->Settled();
    },CefRefPtr<PageAutomation>(this),pending_->token),kSettleMs);
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
      clicked_={number("x"),number("y")};
      if(!world_frame_.number.empty()) { MeasureFrameOffset();return; }
      ClickTarget();
    } else if(action=="press") {
      auto key=CefDictionaryValue::Create();key->SetString("key",pending_->command->GetString("key"));
      for(const char* flag:{"ctrl","shift","alt","meta"}) key->SetBool(flag,pending_->command->GetBool(flag));
      key_(key);
    }
    if(action=="press") pending_->message="Pressed "+Chord();
    else if(action!="click") pending_->message=Text(value,"message");
    SettleAction();
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
  Emit emit_;Click click_;Key key_;
  std::optional<Pending> pending_;Stage stage_=Stage::FrameTree;int call_=0;
  std::vector<TextFrame> text_candidates_;TextFrame text_frame_;std::optional<TextFrame> text_owner_;std::string text_lease_;
  bool native_attempted_=false;std::string native_object_;
  int world_=0,document_=0,main_world_=0;std::string main_id_;
  // The frame's own context and the id it was made for, kept apart from the
  // page's so measuring the frame's place cannot retarget the action.
  std::optional<int> frame_world_;
  // The frame whose isolated world the pending request is creating, empty for
  // the world of the document the action runs in.
  std::string world_request_;
  CefRefPtr<CefDictionaryValue> tree_;uint64_t tree_generation_=~uint64_t{0};uint64_t generation_=0;
  std::string loader_,world_id_;PageFrame world_frame_;Clicked clicked_;uint64_t token_=0,navigations_=0;
  IMPLEMENT_REFCOUNTING(PageAutomation);
};
}
