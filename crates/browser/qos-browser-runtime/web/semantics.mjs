const roles={Window:'group',Group:'group',Generic:'group',Toolbar:'toolbar',TabBar:'tablist',Tab:'tab',TabPanel:'tabpanel',List:'list',ListItem:'listitem',Table:'table',Row:'row',Cell:'cell',Tree:'tree',TreeItem:'treeitem',Menu:'menu',MenuItem:'menuitem',Button:'button',ToggleButton:'button',Checkbox:'checkbox',RadioButton:'radio',Switch:'switch',Slider:'slider',SpinButton:'spinbutton',ComboBox:'combobox',TextInput:'textbox',MultilineTextInput:'textbox',Link:'link',ProgressBar:'progressbar',Spinner:'status',Tooltip:'tooltip',Image:'img',Document:'document',Paragraph:'paragraph',Dialog:'dialog'};
const segments=new Intl.Segmenter(undefined,{granularity:'grapheme'});
let bridgeSequence=0;
const graphemeIndex=(value,offset)=>[...segments.segment(value.slice(0,offset))].length;
function domOffset(value,index){
  if(index<=0)return 0;
  let count=0;
  for(const segment of segments.segment(value)){
    if(count++===index)return segment.index;
  }
  return value.length;
}
export function semanticBridge(root,invoke,report) {
  const elements=new Map();
  const recoverySurfaces=new Set();
  const recoveries=new Map();
  const bridgeId=++bridgeSequence;
  let elementSequence=0;
  let activeDescendantHost;
  let previousActiveDescendant;
  let previousOwns;
  let pendingFocus;
  function clearActiveDescendant(){
    if(!activeDescendantHost)return;
    if(previousActiveDescendant===null)activeDescendantHost.removeAttribute('aria-activedescendant');else activeDescendantHost.setAttribute('aria-activedescendant',previousActiveDescendant);
    if(previousOwns===null)activeDescendantHost.removeAttribute('aria-owns');else activeDescendantHost.setAttribute('aria-owns',previousOwns);
    activeDescendantHost=undefined;previousActiveDescendant=undefined;previousOwns=undefined;
  }
  function setActiveDescendant(host,element){
    if(activeDescendantHost===host&&host.getAttribute('aria-activedescendant')===element.id)return;
    clearActiveDescendant();
    activeDescendantHost=host;
    previousActiveDescendant=host.getAttribute('aria-activedescendant');
    previousOwns=host.getAttribute('aria-owns');
    host.setAttribute('aria-activedescendant',element.id);
    host.setAttribute('aria-owns',[previousOwns,element.id].filter(Boolean).join(' '));
  }
  function act(element,action,value='',anchor=0,focus=0){
    const state=element.qosState;
    if(!state?.active||elements.get(state.key)!==element||!element.route){
      report({semanticAction:action,semanticResult:'stale-target'});return false;
    }
    if(state.disabled||(state.readOnly&&['value','selection'].includes(action))){report({semanticAction:action,semanticResult:'rejected'});return false;}
    try{
      const accepted=invoke(element.route.owner,element.route.target,action,value,anchor,focus)===true;
      report({semanticAction:action,semanticResult:accepted?'accepted':'rejected'});
      return accepted;
    }catch(error){report({semanticAction:action,semanticResult:String(error)});return false;}
  }
  function selection(element){
    const value=element.value;
    return [graphemeIndex(value,element.selectionStart??0),graphemeIndex(value,element.selectionEnd??0)];
  }
  function dispatchSelection(element){
    const range=selection(element);
    if(element.qosState.lastSelection?.[0]===range[0]&&element.qosState.lastSelection?.[1]===range[1])return true;
    if(!act(element,'selection','',range[0],range[1]))return false;
    element.qosState.lastSelection=range;return true;
  }
  function restoreCanonical(element){
    const state=element.qosState;
    const scroll=[element.scrollLeft,element.scrollTop];element.value=state.canonicalValue;
    if(state.canonicalSelection)element.setSelectionRange?.(domOffset(element.value,state.canonicalSelection[0]),domOffset(element.value,state.canonicalSelection[1]));
    [element.scrollLeft,element.scrollTop]=scroll;
  }
  function dispatchValue(element){
    if(!act(element,'value',element.value)){restoreCanonical(element);return false;}
    element.qosState.canonicalValue=element.value;
    element.qosState.lastSelection=null;
    dispatchSelection(element);return true;
  }
  function retainedComposition(base,current,data){
    if(typeof data==='string'&&data.length)return data;
    let prefix=0;
    while(prefix<base.length&&prefix<current.length&&base[prefix]===current[prefix])prefix++;
    let suffix=0;
    while(suffix<base.length-prefix&&suffix<current.length-prefix&&base[base.length-1-suffix]===current[current.length-1-suffix])suffix++;
    return current.slice(prefix,current.length-suffix);
  }
  function clearRecovery(recovery){
    recoveries.delete(recovery.key);
    recoverySurfaces.delete(recovery.surface);recovery.surface.remove();
  }
  function recoveryTarget(recovery){
    const element=elements.get(recovery.key);
    return element?.qosEditable&&element.qosState.active&&!element.qosState.disabled&&!element.qosState.readOnly?element:undefined;
  }
  function syncRecovery(recovery){
    const available=Boolean(recoveryTarget(recovery));
    recovery.recover.disabled=!available;
    recovery.explanation.textContent=available
      ?'Input changed elsewhere. Your composed text was retained.'
      :'The original input is unavailable. Your composed text remains available to copy.';
  }
  function recoverComposition(recovery){
    const element=recoveryTarget(recovery);
    if(!element||!recovery.text)return false;
    const state=element.qosState;
    const value=state.canonicalValue;
    const focus=state.canonicalSelection?.[1]??graphemeIndex(value,value.length);
    const offset=domOffset(value,focus);
    element.value=`${value.slice(0,offset)}${recovery.text}${value.slice(offset)}`;
    const caret=focus+graphemeIndex(recovery.text,recovery.text.length);
    element.setSelectionRange?.(domOffset(element.value,caret),domOffset(element.value,caret));
    if(!dispatchValue(element))return false;
    clearRecovery(recovery);return true;
  }
  function presentRecovery(element,text){
    if(!text)return;
    const state=element.qosState;
    let recovery=recoveries.get(state.key);
    if(!recovery){
      const surface=document.createElement('div');
      const explanation=document.createElement('span');
      const retained=document.createElement('textarea');
      const recover=document.createElement('button');
      const dismiss=document.createElement('button');
      recovery={key:state.key,text:'',surface,explanation,retained,recover};
      surface.dataset.qosCompositionRecovery=state.key;
      surface.setAttribute('role','alert');
      surface.style.cssText='position:absolute;left:12px;bottom:12px;z-index:2147483647;box-sizing:border-box;max-width:calc(100% - 24px);padding:10px 12px;border:1px solid #64748b;border-radius:8px;background:#0f172a;color:#f8fafc;font:13px system-ui;box-shadow:0 4px 18px #0008;';
      explanation.textContent='Input changed elsewhere. Your composed text was retained.';
      retained.readOnly=true;retained.setAttribute('aria-label','Retained composed text');
      retained.style.cssText='display:block;box-sizing:border-box;width:100%;margin:8px 0;padding:6px;color:#0f172a;background:#f8fafc;';
      recover.type='button';recover.textContent='Insert retained text';
      recover.addEventListener('click',()=>recoverComposition(recovery));
      dismiss.type='button';dismiss.textContent='Dismiss retained text';dismiss.style.cssText='margin-left:8px;';
      dismiss.addEventListener('click',()=>clearRecovery(recovery));
      surface.append(explanation,retained,recover,dismiss);
      (root.parentNode??root).append(surface);
      recoveries.set(state.key,recovery);recoverySurfaces.add(surface);
    }
    recovery.text+=text;recovery.retained.value=recovery.text;syncRecovery(recovery);
  }
  function createElement(editable,key,role){
    const element=document.createElement(editable?'textarea':role==='Button'?'button':'div');
    element.id=`qos-semantic-${bridgeId}-${++elementSequence}`;
    element.dataset.qosSemantic=key;
    element.qosEditable=editable;
    element.qosState={key,active:true,disabled:false,readOnly:false,composing:false,canonicalValue:'',compositionBase:null,suppressInput:null,canonicalSelection:null,lastSelection:null,reconcilingFocus:false};
    element.addEventListener('focus',()=>{
      if(!element.qosState.reconcilingFocus&&act(element,'focus'))pendingFocus={key,remainingSnapshots:1};
    });
    element.addEventListener('click',()=>act(element,'activate'));
    if(editable){
      element.addEventListener('input',event=>{
        const state=element.qosState;
        if(state.composing||event.isComposing)return;
        if(state.suppressInput===element.value){state.suppressInput=null;return;}
        state.suppressInput=null;dispatchValue(element);
      });
      element.addEventListener('select',()=>{
        if(element.qosState.composing)return;
        dispatchSelection(element);
      });
      element.addEventListener('compositionstart',()=>{
        const state=element.qosState;
        state.composing=true;state.compositionBase=element.value;state.suppressInput=null;
      });
      element.addEventListener('compositionend',event=>{
        const state=element.qosState;state.composing=false;
        if(state.canonicalValue!==state.compositionBase){
          const retained=retainedComposition(state.compositionBase,element.value,event.data);
          restoreCanonical(element);
          presentRecovery(element,retained);
          report({semanticAction:'value',semanticResult:'composition-cancelled-after-canonical-edit'});
        }else if(dispatchValue(element))state.suppressInput=element.value;
        state.compositionBase=null;
      });
      element.addEventListener('compositioncancel',()=>{
        const state=element.qosState;
        state.composing=false;state.compositionBase=null;restoreCanonical(element);
      });
    }
    return element;
  }
  function update(snapshot){
    const live=new Set();
    const ownedBeforeUpdate=new Set(elements.values());
    const activeBeforeUpdate=document.activeElement;
    let canonicalFocus;
    function semantic(node,owner,parent,index){
      const key=JSON.stringify([owner,node.semantic_key??node.action_target??index]);live.add(key);
      let element=elements.get(key);
      const editable=['TextInput','MultilineTextInput'].includes(node.role);
      if(element&&element.qosEditable!==editable){
        element.qosState.active=false;element.route=undefined;element.remove();elements.delete(key);element=undefined;
      }
      if(!element){
        element=createElement(editable,key,node.role);elements.set(key,element);
      }
      element.route={owner,target:node.action_target};
      element.qosState.disabled=Boolean(node.state?.disabled);
      element.qosState.readOnly=Boolean(node.state?.read_only);
      if(node.state?.focused&&!canonicalFocus)canonicalFocus=element;
      if(element.parentNode!==parent)parent.append(element);
      const role=node.role&&typeof node.role==='object'?(node.role.Heading?'heading':undefined):roles[node.role];
      if(role)element.setAttribute('role',role);else element.removeAttribute('role');
      const level=node.role?.Heading?.level;
      if(level!==undefined)element.setAttribute('aria-level',String(level));else element.removeAttribute('aria-level');
      if(node.name)element.setAttribute('aria-label',node.name);else element.removeAttribute('aria-label');
      if(node.description)element.setAttribute('aria-description',node.description);else element.removeAttribute('aria-description');
      element.tabIndex=node.state?.focusable&&!node.state?.disabled?0:-1;
      for(const flag of ['selected','checked','expanded','disabled','busy','multiline','read_only']){
        const value=node.state?.[flag]; const attribute=`aria-${flag==='read_only'?'readonly':flag}`;
        if(value!==undefined&&value!==null)element.setAttribute(attribute,String(value));else element.removeAttribute(attribute);
      }
      element.disabled=Boolean(node.state?.disabled);
      element.readOnly=Boolean(node.state?.read_only);
      if(node.live)element.setAttribute('aria-live',node.live.toLowerCase());else element.removeAttribute('aria-live');
      const value=node.state?.protected?'':node.value??'';
      if(editable){
        const state=element.qosState;
        state.canonicalValue=value;
        state.canonicalSelection=node.text_selection?[node.text_selection.anchor,node.text_selection.focus]:null;
        if(!state.composing){
          const scroll=[element.scrollLeft,element.scrollTop];
          if(element.value!==value)element.value=value;
          if(state.canonicalSelection){
            element.setSelectionRange?.(domOffset(value,state.canonicalSelection[0]),domOffset(value,state.canonicalSelection[1]));
            state.lastSelection=[...state.canonicalSelection];
          }
          [element.scrollLeft,element.scrollTop]=scroll;
        }
      }
      else if(!node.children?.length)element.textContent=value||node.name||'';
      (node.children??[]).forEach((child,i)=>semantic(child,owner,element,`${index}/${i}`));
    }
    function walk(node,parent){
      if(node.semantics)semantic(node.semantics,node.test_id??node.path,parent,node.path);
      for(const child of node.children??[])walk(child,parent);
    }
    walk(snapshot.root,root);
    for(const [key,element] of elements)if(!live.has(key)){element.qosState.active=false;element.route=undefined;element.remove();elements.delete(key);}
    for(const recovery of recoveries.values())syncRecovery(recovery);
    for(const element of elements.values())element.dataset.qosFocused=String(element===canonicalFocus);
    const active=document.activeElement;
    let awaitingFocus=false;
    if(pendingFocus){
      const target=elements.get(pendingFocus.key);
      if(canonicalFocus||target!==active||target.qosState.disabled){pendingFocus=undefined;}
      else if(pendingFocus.remainingSnapshots>0){pendingFocus.remainingSnapshots--;awaitingFocus=true;}
      else pendingFocus=undefined;
    }
    const focusWasManaged=ownedBeforeUpdate.has(activeBeforeUpdate);
    const focusInMount=Boolean(root.parentNode?.contains?.(active));
    const focusInRecovery=[...recoverySurfaces].some(surface=>surface.contains?.(active));
    if(canonicalFocus&&active!==canonicalFocus&&focusInMount&&!focusWasManaged&&!focusInRecovery){
      setActiveDescendant(active,canonicalFocus);
    }else if(canonicalFocus&&active!==canonicalFocus&&(!active||active===document.body||focusWasManaged)){
      clearActiveDescendant();
      canonicalFocus.qosState.reconcilingFocus=true;
      try{canonicalFocus.focus();}finally{canonicalFocus.qosState.reconcilingFocus=false;}
    }else{
      clearActiveDescendant();
      if(!canonicalFocus&&!awaitingFocus&&ownedBeforeUpdate.has(active)&&active.qosState.active)active.blur?.();
    }
    root.dataset.nodeCount=String(elements.size);
  }
  return {update,close(){pendingFocus=undefined;clearActiveDescendant();for(const element of elements.values()){element.qosState.active=false;element.route=undefined;}for(const surface of recoverySurfaces)surface.remove();recoverySurfaces.clear();recoveries.clear();root.replaceChildren();elements.clear();}};
}
