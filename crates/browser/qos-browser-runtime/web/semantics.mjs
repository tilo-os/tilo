const roles={Window:'group',Group:'group',Generic:'group',Toolbar:'toolbar',TabBar:'tablist',Tab:'tab',TabPanel:'tabpanel',List:'list',ListItem:'listitem',Table:'table',Row:'row',Cell:'cell',Tree:'tree',TreeItem:'treeitem',Menu:'menu',MenuItem:'menuitem',Button:'button',ToggleButton:'button',Checkbox:'checkbox',RadioButton:'radio',Switch:'switch',Slider:'slider',SpinButton:'spinbutton',ComboBox:'combobox',TextInput:'textbox',MultilineTextInput:'textbox',Link:'link',ProgressBar:'progressbar',Spinner:'status',Tooltip:'tooltip',Image:'img',Document:'document',Paragraph:'paragraph',Dialog:'dialog'};
const segments=new Intl.Segmenter(undefined,{granularity:'grapheme'});
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
    return dispatchSelection(element);
  }
  function createElement(editable,key,role){
    const element=document.createElement(editable?'textarea':role==='Button'?'button':'div');
    element.dataset.qosSemantic=key;
    element.qosEditable=editable;
    element.qosState={key,active:true,disabled:false,readOnly:false,composing:false,canonicalValue:'',compositionBase:null,suppressInput:null,canonicalSelection:null,lastSelection:null};
    element.addEventListener('focus',()=>act(element,'focus'));
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
      element.addEventListener('compositionend',()=>{
        const state=element.qosState;state.composing=false;
        if(state.canonicalValue!==state.compositionBase){
          restoreCanonical(element);
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
    root.dataset.nodeCount=String(elements.size);
  }
  return {update,close(){for(const element of elements.values()){element.qosState.active=false;element.route=undefined;}root.replaceChildren();elements.clear();}};
}
