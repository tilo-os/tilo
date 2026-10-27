const roles={Window:'group',Group:'group',Generic:'group',Toolbar:'toolbar',TabBar:'tablist',Tab:'tab',TabPanel:'tabpanel',List:'list',ListItem:'listitem',Table:'table',Row:'row',Cell:'cell',Tree:'tree',TreeItem:'treeitem',Menu:'menu',MenuItem:'menuitem',ListBox:'listbox',ListBoxOption:'option',Button:'button',ToggleButton:'button',Checkbox:'checkbox',RadioButton:'radio',Switch:'switch',Slider:'slider',SpinButton:'spinbutton',ComboBox:'combobox',TextInput:'textbox',MultilineTextInput:'textbox',Link:'link',ProgressBar:'progressbar',Spinner:'status',Tooltip:'tooltip',Image:'img',Document:'document',Paragraph:'paragraph',Dialog:'dialog'};
const keyboardActivatable=new Set(['Tab','TreeItem','MenuItem','ToggleButton','Checkbox','RadioButton','Switch','Link','ListItem','ListBoxOption']);
const segments=new Intl.Segmenter(undefined,{granularity:'grapheme'});
const utf8=new TextEncoder();
let bridgeSequence=0;
const MIRROR_STYLE='position:absolute;box-sizing:border-box;margin:0;padding:0;border:0;pointer-events:none;';
const graphemeIndex=(value,offset)=>[...segments.segment(value.slice(0,offset))].length;
function graphemeIndexFromByteOffset(value,offset){
  const target=Math.max(0,Number(offset)||0);
  let bytes=0,index=0;
  for(const segment of segments.segment(value)){
    const next=bytes+utf8.encode(segment.segment).length;
    if(target<next)return index;
    bytes=next;index++;
    if(target===bytes)return index;
  }
  return index;
}
function domOffset(value,index){
  if(index<=0)return 0;
  let count=0;
  for(const segment of segments.segment(value)){
    if(count++===index)return segment.index;
  }
  return value.length;
}
/**
 * The tree a status's `semanticsPatch` makes of `tree`, the one this page
 * holds as number `sequence` (`ui::headless::write_inspection_patch` says
 * what the operations mean). Undefined when the patch was made for another
 * tree: the page then asks for the whole one. Only the nodes the patch
 * touches and their ancestors are new objects; every other node is the one
 * `tree` had, so the mirror can tell it is unchanged by identity alone.
 */
export function applySemanticsPatch(tree,sequence,patch){
  if(!tree||sequence===undefined||sequence!==patch?.base)return undefined;
  const fresh=new Set();
  const copy=value=>{
    if(value===null||typeof value!=='object'||fresh.has(value))return value;
    const next=Array.isArray(value)?[...value]:{...value};fresh.add(next);return next;
  };
  const next=copy(tree);
  next.viewport=patch.viewport;next.keyboard_focus_owner=patch.keyboard_focus_owner;
  for(const op of patch.ops??[]){
    const path=op.p;
    // The slot holding the node: `holder[slot]`, every holder on the way a copy.
    let holder=next,slot=path[0];
    for(const step of path.slice(1)){
      const node=copy(holder[slot]);holder[slot]=node;
      if(step==='s'){holder=node;slot='semantics';}
      else{node.children=copy(node.children);holder=node.children;slot=step;}
    }
    if('full' in op){holder[slot]=op.full;continue;}
    const node=copy(holder[slot]);holder[slot]=node;
    if('own' in op){
      // A retained widget's semantics, like every node's children, are
      // patched apart from its own fields.
      const children=node.children,semantics=node.semantics,widget=!path.includes('s');
      for(const key of Object.keys(node))delete node[key];
      Object.assign(node,op.own,{children});
      if(widget)node.semantics=semantics;
    }else if('splice' in op){
      const [at,removed,added]=op.splice;
      node.children=copy(node.children);node.children.splice(at,removed,...added);
    }
  }
  return next;
}
export function semanticBridge(root,invoke,report,shortcuts,inputScope) {
  const elements=new Map();
  const recoverySurfaces=new Set();
  const recoveries=new Map();
  const bridgeId=++bridgeSequence;
  let elementSequence=0;
  let activeDescendantHost;
  // The focused control whose own aria-activedescendant names the item the
  // keyboard is on inside it (an open combobox's highlighted option).
  let ownedActiveHost;
  let previousActiveDescendant;
  let previousOwns;
  let pendingFocus;
  // CSS pixels per layout pixel of the frame the canvas shows, on each axis.
  let scale=[1,1];
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
  function authorized(element){
    const state=element.tiloState;if(!state.active||elements.get(state.key)!==element)return false;
    const owned=!inputScope||inputScope.owns(element.tiloOwnerPath);
    return owned&&state.active&&elements.get(state.key)===element;
  }
  function currentComposition(element){
    const state=element.tiloState;
    if(state.compositionScope&&!inputScope.valid(state.compositionScope)){state.composing=false;state.compositionScope=null;state.canceledComposition=true;state.compositionBase=null;restoreCanonical(element);replaceCanceledEditor(element);}
    return !state.canceledComposition;
  }
  function replaceCanceledEditor(element){
    const state=element.tiloState;
    if(!state.active||elements.get(state.key)!==element)return;
    const next=createElement(true,state.key,element.tiloRole);
    for(const name of element.getAttributeNames())if(name!=='id')next.setAttribute(name,element.getAttribute(name));
    next.style.cssText=element.style.cssText;next.tabIndex=element.tabIndex;next.disabled=element.disabled;next.readOnly=element.readOnly;
    next.tiloRect=element.tiloRect;next.route=element.route;next.tiloRole=element.tiloRole;next.tiloAncestors=element.tiloAncestors;next.tiloOwnerPath=element.tiloOwnerPath;
    Object.assign(next.tiloState,state,{composing:false,compositionScope:null,compositionBase:null,canceledComposition:false,suppressInput:null});
    state.active=false;element.route=undefined;elements.set(state.key,next);restoreCanonical(next);
    const focused=document.activeElement===element;element.replaceWith(next);if(next.tiloRect)place(next);
    if(focused&&authorized(next)){next.tiloState.reconcilingFocus=true;try{next.focus({preventScroll:true});}finally{next.tiloState.reconcilingFocus=false;}}
  }
  function revokeInput(){
    pendingFocus=undefined;clearActiveDescendant();
    for(const element of [...elements.values()])if(element.tiloEditable)replaceCanceledEditor(element);
  }
  function act(element,action,value='',anchor=0,focus=0){
    const state=element.tiloState;
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
    const start=graphemeIndex(value,element.selectionStart??0),end=graphemeIndex(value,element.selectionEnd??0);
    return element.selectionDirection==='backward'?[end,start]:[start,end];
  }
  function setCanonicalSelection(element,canonicalSelection){
    if(!canonicalSelection)return;
    const [anchor,focus]=canonicalSelection;
    const anchorOffset=domOffset(element.value,anchor),focusOffset=domOffset(element.value,focus);
    element.setSelectionRange?.(
      Math.min(anchorOffset,focusOffset),Math.max(anchorOffset,focusOffset),
      anchor===focus?'none':anchor<focus?'forward':'backward',
    );
  }
  function dispatchSelection(element){
    if(!authorized(element)||!currentComposition(element)){restoreCanonical(element);return false;}
    const range=selection(element);
    if(element.tiloState.lastSelection?.[0]===range[0]&&element.tiloState.lastSelection?.[1]===range[1])return true;
    if(!act(element,'selection','',range[0],range[1]))return false;
    element.tiloState.lastSelection=range;return true;
  }
  function restoreCanonical(element){
    const state=element.tiloState;
    const scroll=[element.scrollLeft,element.scrollTop];element.value=state.canonicalValue;
    setCanonicalSelection(element,state.canonicalSelection);
    [element.scrollLeft,element.scrollTop]=scroll;
  }
  function dispatchValue(element){
    if(!authorized(element)||!currentComposition(element)){restoreCanonical(element);return false;}
    if(!act(element,'value',element.value)){restoreCanonical(element);return false;}
    element.tiloState.canonicalValue=element.value;
    element.tiloState.lastSelection=null;
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
    return element?.tiloEditable&&element.tiloState.active&&!element.tiloState.disabled&&!element.tiloState.readOnly?element:undefined;
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
    const state=element.tiloState;
    const value=state.canonicalValue;
    const focus=state.canonicalSelection?.[1]??graphemeIndex(value,value.length);
    const offset=domOffset(value,focus);
    element.value=`${value.slice(0,offset)}${recovery.text}${value.slice(offset)}`;
    const caret=focus+graphemeIndex(recovery.text,recovery.text.length);
    setCanonicalSelection(element,[caret,caret]);
    if(!dispatchValue(element))return false;
    clearRecovery(recovery);return true;
  }
  function presentRecovery(element,text){
    if(!text)return;
    const state=element.tiloState;
    let recovery=recoveries.get(state.key);
    if(!recovery){
      const surface=document.createElement('div');
      const explanation=document.createElement('span');
      const retained=document.createElement('textarea');
      const recover=document.createElement('button');
      const dismiss=document.createElement('button');
      recovery={key:state.key,text:'',surface,explanation,retained,recover};
      surface.dataset.tiloCompositionRecovery=state.key;
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
    element.id=`tilo-semantic-${bridgeId}-${++elementSequence}`;
    element.dataset.tiloSemantic=key;
    element.tiloEditable=editable;
    // Laid out over the drawn control so magnifiers, touch exploration and
    // element geometry find it there; never painted and never hit by the
    // pointer, which belongs to the canvas underneath.
    element.style.cssText=MIRROR_STYLE;
    element.tiloState={key,active:true,disabled:false,readOnly:false,composing:false,canonicalValue:'',compositionBase:null,suppressInput:null,canonicalSelection:null,lastSelection:null,reconcilingFocus:false};
    element.addEventListener('focus',()=>{
      if(!element.tiloState.reconcilingFocus&&act(element,'focus')){inputScope?.capture();pendingFocus={key,remainingSnapshots:1};}
    });
    element.addEventListener('click',event=>{
      if(event.target!==element)return;
      event.stopPropagation();act(element,'activate');
    });
    if(!editable&&role!=='Button')element.addEventListener('keydown',event=>{
      if(event.target!==element||document.activeElement!==element||!keyboardActivatable.has(element.tiloRole)
        ||event.isComposing||event.repeat||event.ctrlKey||event.altKey||event.metaKey||event.shiftKey
        ||!['Enter',' '].includes(event.key))return;
      event.preventDefault();event.stopPropagation();act(element,'activate');
    });
    if(editable){
      element.addEventListener('keydown',event=>{
        const state=element.tiloState;
        if(!state.active||elements.get(state.key)!==element||!element.route||state.disabled||state.readOnly||document.activeElement!==element)return;
        currentComposition(element);
        if(!event.isComposing&&!state.composing)state.canceledComposition=false;
        if(!authorized(element)||state.canceledComposition){event.preventDefault();restoreCanonical(element);return;}
        shortcuts?.keydown?.(element,event,state.composing,undefined,()=>inputScope&&!inputScope.capture()?.legacy?authorized(element):act(element,'focus'));
      });
      element.addEventListener('keyup',event=>{if(authorized(element))shortcuts?.keyup?.(element,event);});
      element.addEventListener('blur',()=>{if(authorized(element))shortcuts?.blur?.(element);});
      element.addEventListener('input',event=>{
        const state=element.tiloState;
        if(!authorized(element)||!currentComposition(element)){restoreCanonical(element);return;}
        if(state.composing||event.isComposing)return;
        if(state.suppressInput===element.value){state.suppressInput=null;return;}
        state.suppressInput=null;dispatchValue(element);
      });
      element.addEventListener('select',()=>{
        if(element.tiloState.composing)return;
        dispatchSelection(element);
      });
      element.addEventListener('compositionstart',()=>{
        const state=element.tiloState;
        if(!authorized(element)){state.canceledComposition=true;restoreCanonical(element);return;}
        state.compositionScope=inputScope?.capture();state.canceledComposition=false;
        state.composing=true;state.compositionBase=element.value;state.suppressInput=null;
      });
      element.addEventListener('compositionend',event=>{
        const state=element.tiloState;
        if(!authorized(element)||!currentComposition(element)){
          state.composing=false;state.compositionScope=null;state.compositionBase=null;state.canceledComposition=true;restoreCanonical(element);
          replaceCanceledEditor(element);return;
        }
        state.composing=false;state.compositionScope=null;
        if(state.canonicalValue!==state.compositionBase){
          const retained=retainedComposition(state.compositionBase,element.value,event.data);
          restoreCanonical(element);
          presentRecovery(element,retained);
          report({semanticAction:'value',semanticResult:'composition-cancelled-after-canonical-edit'});
        }else if(dispatchValue(element))state.suppressInput=element.value;
        state.compositionBase=null;
      });
      element.addEventListener('compositioncancel',()=>{
        const state=element.tiloState;
        state.composing=false;state.compositionBase=null;restoreCanonical(element);
      });
    }
    return element;
  }
  // Each mirrored element sits at its node's drawn rect: layout coordinates
  // scaled to the canvas's CSS box, offset from the mirrored parent it is
  // nested in (every one is absolutely positioned, so that parent is its
  // containing block) or from the mirror's root, which covers the canvas.
  function place(element){
    const rect=element.tiloRect,parent=element.parentNode?.tiloRect;
    const x=rect[0]*scale[0],y=rect[1]*scale[1],px=parent?parent[0]*scale[0]:0,py=parent?parent[1]*scale[1]:0;
    const left=x-px,top=y-py,width=Math.max(0,rect[2]*scale[0]),height=Math.max(0,rect[3]*scale[1]);
    const placed=element.tiloPlaced;
    if(placed&&placed[0]===left&&placed[1]===top&&placed[2]===width&&placed[3]===height)return;
    element.tiloPlaced=[left,top,width,height];
    const style=element.style;
    style.left=`${left}px`;style.top=`${top}px`;
    style.width=`${width}px`;style.height=`${height}px`;
  }
  function layout(cssSize,frameSize){
    const next=[0,1].map(axis=>cssSize?.[axis]>0&&frameSize?.[axis]>0?cssSize[axis]/frameSize[axis]:1);
    if(next[0]===scale[0]&&next[1]===scale[1])return;
    scale=next;for(const element of elements.values())place(element);
  }
  function update(snapshot){
    const live=new Set();
    const ownedBeforeUpdate=new Set(elements.values());
    const activeBeforeUpdate=document.activeElement;
    let canonicalFocus;
    const focusedCandidates=[];
    const activeCandidates=[];
    // A test ID is a convenient alias only when it names one current widget.
    // Keep DOM identity separate from the route, which changes with layout paths.
    const testIds=new Map(),semanticOwners=new Map();
    function countOwners(node){
      if(node.test_id)testIds.set(node.test_id,(testIds.get(node.test_id)??0)+1);
      const key=node.semantics?.semantic_key;
      if(key)semanticOwners.set(key,(semanticOwners.get(key)??0)+1);
      for(const child of node.children??[])countOwners(child);
    }
    countOwners(snapshot.root);
    function semantic(node,owner,parent,index,identity,ancestry,ownerPath,enclosing){
      // A hidden node (a closed dialog) is not there for assistive
      // technology, and neither is anything inside it.
      if(node.state?.hidden)return;
      const key=JSON.stringify([identity,node.semantic_key??node.action_target??index]);live.add(key);
      let element=elements.get(key);
      if(element?.tiloEditable&&!currentComposition(element))element=elements.get(key);
      const editable=['TextInput','MultilineTextInput'].includes(node.role);
      if(element&&element.tiloEditable!==editable){
        shortcuts?.blur?.(element);element.tiloState.active=false;element.route=undefined;element.remove();elements.delete(key);element=undefined;
      }
      if(!element){
        element=createElement(editable,key,node.role);elements.set(key,element);
      }
      element.route={owner,target:node.action_target};
      element.tiloRole=node.role;
      element.tiloAncestors=ancestry;
      element.tiloOwnerPath=ownerPath;
      const disabled=Boolean(node.state?.disabled),readOnly=Boolean(node.state?.read_only);
      if(editable&&!element.tiloState.disabled&&!element.tiloState.readOnly&&(disabled||readOnly))shortcuts?.blur?.(element);
      element.tiloState.disabled=disabled;
      element.tiloState.readOnly=readOnly;
      if(node.state?.focused&&!disabled)focusedCandidates.push(element);
      if(node.state?.active_descendant&&!disabled)activeCandidates.push(element);
      if(element.parentNode!==parent)parent.append(element);
      // A node without bounds of its own stands for the area it is drawn in.
      const bounds=node.bounds;
      element.tiloRect=bounds?[bounds.x0,bounds.y0,bounds.x1-bounds.x0,bounds.y1-bounds.y0]:enclosing??[0,0,0,0];
      place(element);
      // A node a patch left alone is the very object the element last
      // showed (applySemanticsPatch keeps it), so it says nothing new.
      const changed=element.tiloNode!==node;element.tiloNode=node;
      if(changed){
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
      }
      const value=node.state?.protected?'':node.value??'';
      if(editable){
        const state=element.tiloState;
        state.canonicalValue=value;
        state.canonicalSelection=node.text_selection?[
          graphemeIndexFromByteOffset(value,node.text_selection.anchor),
          graphemeIndexFromByteOffset(value,node.text_selection.focus),
        ]:null;
        if(!state.composing){
          const scroll=[element.scrollLeft,element.scrollTop];
          if(element.value!==value)element.value=value;
          if(state.canonicalSelection){
            setCanonicalSelection(element,state.canonicalSelection);
            state.lastSelection=[...state.canonicalSelection];
          }
          [element.scrollLeft,element.scrollTop]=scroll;
        }
      }
      else if(changed&&!node.children?.length)element.textContent=value||node.name||'';
      (node.children??[]).forEach((child,i)=>semantic(child,owner,element,`${index}/${i}`,identity,[...ancestry,key],ownerPath,element.tiloRect));
    }
    function walk(node,parent,ancestry=[]){
      if(node.semantics?.state?.hidden)return;
      if(node.semantics){
        const uniqueTestId=node.test_id&&testIds.get(node.test_id)===1;
        const semanticKey=node.semantics.semantic_key;
        const identity=semanticKey&&semanticOwners.get(semanticKey)===1?['semantic',semanticKey]:uniqueTestId?['test',node.test_id]:['path',node.path];
        semantic(node.semantics,node.path,parent,node.path,identity,ancestry,node.path,node.rect);
        ancestry=[...ancestry,JSON.stringify([identity,node.semantics.semantic_key??node.semantics.action_target??node.path])];
      }
      for(const child of node.children??[])walk(child,parent,ancestry);
    }
    walk(snapshot.root,root);
    for(const [key,element] of elements)if(!live.has(key)){shortcuts?.blur?.(element);element.tiloState.active=false;element.route=undefined;element.remove();elements.delete(key);}
    for(const recovery of recoveries.values())syncRecovery(recovery);
    // Retained owners and their editable/synthetic children may both report
    // focus. An explicitly focused descendant is the more precise contract.
    // Apps retain local editor focus even when another app owns the keyboard.
    // The runner's exact current inspection path disambiguates those owners.
    // Explicit null is authoritative no-owner; only omitted legacy metadata
    // may fall back to independently retained local focus.
    const ownership=inputScope?.capture();
    const keyboardOwner=inputScope&&!ownership?.legacy?(ownership?.owner??null):snapshot.keyboard_focus_owner;
    const ownedCandidates=focusedCandidates.filter(element=>element.tiloOwnerPath===keyboardOwner);
    const candidates=keyboardOwner!==undefined?ownedCandidates:focusedCandidates;
    canonicalFocus=candidates.find(candidate=>!candidates.some(other=>other.tiloAncestors.includes(candidate.tiloState.key)));

    const active=document.activeElement;
    let awaitingFocus=false;
    if(pendingFocus){
      const target=elements.get(pendingFocus.key);
      if(!target||target!==active||target.tiloState.disabled||target.tiloState.readOnly){pendingFocus=undefined;}
      else if(canonicalFocus){
        // An owner acknowledges focus on its live synthetic leaf without
        // claiming the leaf's DOM focus. Never extend this across owners.
        if(target===canonicalFocus||(target.route.owner===canonicalFocus.route.owner&&target.tiloAncestors.includes(canonicalFocus.tiloState.key))){
          canonicalFocus=target;pendingFocus.remainingSnapshots=0;
        }else pendingFocus=undefined;
      }else if(pendingFocus.remainingSnapshots>0){pendingFocus.remainingSnapshots--;awaitingFocus=true;}
      else pendingFocus=undefined;
    }
    for(const element of elements.values())element.dataset.tiloFocused=String(element===canonicalFocus);
    // Focus stays on the control; the item the keyboard is on inside it is
    // its active descendant, announced without moving focus.
    const activeItem=canonicalFocus&&activeCandidates.find(candidate=>candidate.tiloAncestors.includes(canonicalFocus.tiloState.key));
    if(ownedActiveHost&&(ownedActiveHost!==canonicalFocus||!activeItem))ownedActiveHost.removeAttribute('aria-activedescendant');
    ownedActiveHost=undefined;
    if(activeItem){canonicalFocus.setAttribute('aria-activedescendant',activeItem.id);ownedActiveHost=canonicalFocus;}
    const focusWasManaged=ownedBeforeUpdate.has(activeBeforeUpdate);
    const focusInMount=Boolean(root.parentNode?.contains?.(active));
    const focusInRecovery=[...recoverySurfaces].some(surface=>surface.contains?.(active));
    if(canonicalFocus&&active!==canonicalFocus&&focusInMount&&!focusWasManaged&&!focusInRecovery){
      setActiveDescendant(active,activeItem??canonicalFocus);
    }else if(canonicalFocus&&active!==canonicalFocus&&(!active||active===document.body||focusWasManaged)){
      clearActiveDescendant();
      canonicalFocus.tiloState.reconcilingFocus=true;
      try{canonicalFocus.focus({preventScroll:true});}finally{canonicalFocus.tiloState.reconcilingFocus=false;}
    }else{
      clearActiveDescendant();
      if(!canonicalFocus&&!awaitingFocus&&ownedBeforeUpdate.has(active)&&active.tiloState.active)active.blur?.();
    }
    root.dataset.nodeCount=String(elements.size);
  }
  return {update,layout,revokeInput,close(){pendingFocus=undefined;clearActiveDescendant();ownedActiveHost?.removeAttribute('aria-activedescendant');ownedActiveHost=undefined;for(const element of elements.values()){shortcuts?.blur?.(element);element.tiloState.active=false;element.route=undefined;}for(const surface of recoverySurfaces)surface.remove();recoverySurfaces.clear();recoveries.clear();root.replaceChildren();elements.clear();}};
}
