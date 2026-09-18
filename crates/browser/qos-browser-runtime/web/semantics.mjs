const roles={Window:'group',Group:'group',Generic:'group',Toolbar:'toolbar',TabBar:'tablist',Tab:'tab',TabPanel:'tabpanel',List:'list',ListItem:'listitem',Table:'table',Row:'row',Cell:'cell',Tree:'tree',TreeItem:'treeitem',Menu:'menu',MenuItem:'menuitem',Button:'button',ToggleButton:'button',Checkbox:'checkbox',RadioButton:'radio',Switch:'switch',Slider:'slider',SpinButton:'spinbutton',ComboBox:'combobox',TextInput:'textbox',MultilineTextInput:'textbox',Link:'link',ProgressBar:'progressbar',Spinner:'status',Tooltip:'tooltip',Image:'img',Document:'document',Paragraph:'paragraph',Dialog:'dialog'};
export function semanticBridge(root,invoke,report) {
  const elements=new Map();
  function update(snapshot){
    const live=new Set();
    function semantic(node,owner,parent,index){
      const key=JSON.stringify([owner,node.semantic_key??node.action_target??index]);live.add(key);
      let element=elements.get(key);
      const editable=['TextInput','MultilineTextInput'].includes(node.role);
      if(!element){
        element=document.createElement(editable?'textarea':node.role==='Button'?'button':'div');
        element.dataset.qosSemantic=key;elements.set(key,element);
        element.addEventListener('focus',()=>act(element,'focus'));
        element.addEventListener('click',()=>act(element,'activate'));
        if(editable){
          element.addEventListener('change',()=>act(element,'value',element.value));
          element.addEventListener('select',()=>{
            const segments=new Intl.Segmenter(undefined,{granularity:'grapheme'});
            const count=n=>[...segments.segment(element.value.slice(0,n))].length;
            act(element,'selection','',count(element.selectionStart),count(element.selectionEnd));
          });
        }
      }
      element.route={owner,target:node.action_target};
      if(element.parentNode!==parent)parent.append(element);
      const role=typeof node.role==='object'?'heading':roles[node.role];
      if(role)element.setAttribute('role',role);
      if(node.name)element.setAttribute('aria-label',node.name);else element.removeAttribute('aria-label');
      if(node.description)element.setAttribute('aria-description',node.description);
      if(node.state?.focusable)element.tabIndex=0;
      for(const flag of ['selected','checked','expanded','disabled','busy','multiline','read_only']){
        const value=node.state?.[flag]; const attribute=`aria-${flag==='read_only'?'readonly':flag}`;
        if(value!==undefined&&value!==null)element.setAttribute(attribute,String(value));else element.removeAttribute(attribute);
      }
      if(node.live)element.setAttribute('aria-live',node.live.toLowerCase());
      const value=node.state?.protected?'':node.value??'';
      if(editable){if(document.activeElement!==element)element.value=value;}
      else if(!node.children?.length)element.textContent=value||node.name||'';
      (node.children??[]).forEach((child,i)=>semantic(child,owner,element,`${index}/${i}`));
    }
    function walk(node,parent){
      if(node.semantics)semantic(node.semantics,node.test_id??node.path,parent,node.path);
      for(const child of node.children??[])walk(child,parent);
    }
    walk(snapshot.root,root);
    for(const [key,element] of elements)if(!live.has(key)){element.remove();elements.delete(key);}
    root.dataset.nodeCount=String(elements.size);
  }
  function act(element,action,value='',anchor=0,focus=0){
    try{invoke(element.route.owner,element.route.target,action,value,anchor,focus);report({semanticAction:action,semanticResult:'accepted'});}
    catch(error){report({semanticAction:action,semanticResult:String(error)});}
  }
  return {update,close(){root.replaceChildren();elements.clear();}};
}
