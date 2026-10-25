#pragma once
#include "page_automation_script.h"
#include "include/cef_browser.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_dialog_handler.h"
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
// Documents are numbered across every tab of this host, so a ref read in one
// tab never resolves in another.
inline int page_documents=0;

// Browser-process page reading and operation for the browser's page actions.
// Script runs in an isolated world of the main frame, created through
// DevTools: it shares the page's DOM, but page script can neither see nor
// call it. Clicks and keys go through the same CEF input path as a person's,
// so the page receives trusted events with user activation.
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
  PageAutomation(CefRefPtr<CefBrowser> browser,Emit emit,Click click,Key key)
      :browser_(browser),emit_(std::move(emit)),click_(std::move(click)),key_(std::move(key)) {}
  void Attach() { registration_=browser_->GetHost()->AddDevToolsMessageObserver(this); }
  void Detach() { registration_=nullptr;browser_=nullptr;pending_.reset();CancelFileDialog(); }
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
    if(pending_ && pending_->awaiting_start) { pending_->awaiting_start=false;pending_->awaiting_load=true; }
  }
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
    pending_=Pending{request,++token_,navigations_,false,owned};
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
    world_=0;loader_.clear();text_owner_.reset();text_lease_.clear();
    if(pending_ && call_) Fail("unavailable","The page's inspector connection closed");
  }

 private:
  static constexpr int64_t kTimeoutMs=30000;
  // A started navigation is awaited this long; a slower one is reported as
  // still loading and the caller waits for it.
  static constexpr int64_t kLoadWaitMs=10000;
  // A held file chooser waits this long for `fill_files`, then is cancelled so
  // the page is never left waiting on input that will not come.
  static constexpr int64_t kFileDialogWaitMs=60000;
  enum class Stage { FrameTree, World, TextWorld, TextFocus, TextFailureFrameTree, NativeElement, NativeDOM, NativeDocument, NativeNode, NativeEditors, NativeResolve, NativeConfigure, FileTarget, FileSet, Evaluate, Fence };
  struct TextFrame { std::string id,loader;int world=0,document=0;bool same_origin=true; };
  struct Pending {
    int request;uint64_t token;uint64_t navigations;bool retried;
    CefRefPtr<CefDictionaryValue> command;std::string message;
    // "none": the action ran no page code. "started": the page began leaving
    // the document while it handled the action's input. "possible": it had
    // not, and may still navigate later on its own.
    std::string navigation="none";bool awaiting_start=false,awaiting_load=false;
  };
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
    // A press with no element arms a key: nothing on the page is read first.
    if(Action()=="press" && !pending_->command->HasKey("element")) { Act(nullptr);return; }
    if(Action()=="press" && !pending_->command->HasKey("element")) { Act(nullptr);return; }
    // A chooser this page is waiting on is answered with the page's own
    // completion path, so its input and change events are the picker's, and
    // no DOM node has to be reached while that dialog is open.
    if(Action()=="fill_files" && file_dialog_) { AnswerFileDialog(pending_->command->GetList("paths"));return; }
    // Choosing files resolves the ref exactly as reading the page did, in the
    // document the outline named.
    Call(Stage::FrameTree,"Page.getFrameTree",nullptr);
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
    Call(Stage::FileTarget,"Runtime.evaluate",params);
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
        stage_==Stage::NativeNode || stage_==Stage::NativeEditors || stage_==Stage::NativeResolve || stage_==Stage::NativeConfigure ||
        stage_==Stage::FileTarget || stage_==Stage::FileSet) {
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
      if(world_ && loader==loader_) { Evaluate();return; }
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
      document_=page_documents=page_documents%2147483647+1;
      Evaluate();
    } else if(stage_==Stage::Fence) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      auto value=remote && remote->GetType("value")==VTYPE_DICTIONARY?remote->GetDictionary("value"):nullptr;
      // No answer means the document went away under the fence: a navigation
      // that began during the action has committed.
      Fenced(value?Text(value,"navigation"):navigations_!=pending_->navigations?"started":"possible");
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
    if(Action()=="fill_files") { FileResult(success,result);return; }
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
  // The file input's files, in two steps: the isolated world's object for the
  // element (already validated by `fillFilesTarget`), then the files
  // themselves. `DOM.setFileInputFiles` is the browser's own
  // Enter-on-a-chooser path: the page receives trusted input and change
  // events, and scripts it runs see the files a person would have chosen. The
  // element is its object, never a node id: a node id belongs to the
  // inspected document, and no agent is attached for this world's document.
  void FileResult(bool success,CefRefPtr<CefDictionaryValue> result) {
    auto params=CefDictionaryValue::Create();
    if(stage_==Stage::FileTarget) {
      auto remote=success && result?result->GetDictionary("result"):nullptr;
      // The element arrives as the object itself: an object is sent with its
      // own properties, so a wrapper would arrive without the element in it.
      if(!remote || remote->GetType("objectId")!=VTYPE_STRING) { Fail("page_changed","The file input is no longer on the page");return; }
      file_object_=remote->GetString("objectId").ToString();
      params->SetString("objectId",file_object_);
      params->SetList("files",pending_->command->GetList("paths")->Copy());
      Call(Stage::FileSet,"DOM.setFileInputFiles",params);
    } else {
      if(!success) { Fail("not_set","The page refused the chosen files");return; }
      auto data=CefDictionaryValue::Create();data->SetList("files",FileNames());data->SetString("mode","dom");
      Succeed(pending_->message,std::nullopt,data);return;
    }
  }
  void Evaluate() {
    auto command=CefDictionaryValue::Create();
    const auto action=Action();
    const bool elementless=!pending_->command->HasKey("element");
    command->SetString("action",action=="click"?"locate":action=="press"?(elementless?"arm":"focus"):action);
    command->SetInt("doc",PrivateText()?text_frame_.document:document_);
    for(const char* key:{"element","text","key","lease","history_scope"}) if(pending_->command->HasKey(key)) command->SetString(key,pending_->command->GetString(key));
    for(const char* key:{"cross_origin","secret","native_caret","enabled"}) if(pending_->command->HasKey(key)) command->SetBool(key,pending_->command->GetBool(key));
    for(const char* key:{"x","y"}) if(pending_->command->HasKey(key)) command->SetDouble(key,Number(pending_->command,key));
    for(const char* key:{"quiet_ms","timeout_ms"}) if(pending_->command->HasKey(key)) command->SetInt(key,pending_->command->GetInt(key));
    if(pending_->command->HasKey("expires_ms")) command->SetDouble("expires_ms",Number(pending_->command,"expires_ms"));
    if(pending_->command->HasKey("values")) command->SetList("values",pending_->command->GetList("values")->Copy());
    if(pending_->command->HasKey("paths")) command->SetList("paths",pending_->command->GetList("paths")->Copy());
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
    // An edit runs the page's input handlers.
    if(action=="commit_text_input" || action=="undo_text_input" || action=="redo_text_input") pending_->navigation="possible";
    if(action=="outline" || action=="changes") { Succeed(Text(value,"message"),Text(value,"outline"));return; }
    if(action=="fill_files") { FileTarget(value);return; }
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
    // Answers once the page has handled the input: see `fence` in page_automation.js.
    auto params=CefDictionaryValue::Create();
    params->SetString("expression","globalThis.__tiloPage.fence()");
    params->SetInt("contextId",world_);params->SetBool("returnByValue",true);
    params->SetBool("awaitPromise",true);params->SetBool("silent",true);
    Call(Stage::Fence,"Runtime.evaluate",params);
  }
  void Fenced(const std::string& navigation) {
    pending_->navigation=navigation=="started"?"started":"possible";
    // A click that opened the page's file chooser is held, not shown: say so,
    // so the caller knows the page is waiting and how to answer it.
    if(file_dialog_)
      pending_->message+="; the page opened a file chooser, which is answered by fill-page-files for that element";
    if(pending_->navigation!="started") { Succeed(pending_->message,std::nullopt);return; }
    if(navigations_==pending_->navigations) pending_->awaiting_start=true;
    else if(browser_->IsLoading()) pending_->awaiting_load=true;
    else { Succeed(pending_->message,std::nullopt);return; }
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
    // A navigation the page asked for that the browser never began is not one.
    const bool navigated=pending_->navigation=="started" && navigations_!=pending_->navigations;
    fields->SetString("navigation",navigated?"started":pending_->navigation=="none"?"none":"possible");
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
  void Reply(int request,CefRefPtr<CefDictionaryValue> fields,bool text_input=false) { fields->SetInt("request_id",request);emit_(text_input?"text_input_result":"page_result",fields); }
  CefRefPtr<CefBrowser> browser_;CefRefPtr<CefRegistration> registration_;
  Emit emit_;Click click_;Key key_;
  std::optional<Pending> pending_;Stage stage_=Stage::FrameTree;int call_=0;
  std::vector<TextFrame> text_candidates_;TextFrame text_frame_;std::optional<TextFrame> text_owner_;std::string text_lease_;
  bool native_attempted_=false;std::string native_object_;
  std::string file_object_;
  CefRefPtr<CefFileDialogCallback> file_dialog_;int file_dialog_request_=0;
  int world_=0,document_=0;std::string loader_;uint64_t token_=0,navigations_=0;
  IMPLEMENT_REFCOUNTING(PageAutomation);
};
}
