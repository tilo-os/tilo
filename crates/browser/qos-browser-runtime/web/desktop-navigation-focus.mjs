/** DOM policy owned by one mount, separate from the navigation projection. */
export function createNavigationFocus({document,surface,canvas,active}) {
 let closed=false,pending,opener,errorCount=0;
 const visible=element=>Boolean(element?.isConnected&&!element.disabled&&!element.inert&&element.getClientRects().length);
 const available=()=>!closed&&active()&&visible(surface)&&visible(canvas);
 const matches=(request,snapshot)=>request.generation===snapshot.generation&&request.workspace===snapshot.activeWorkspace;
 function clear(){pending=undefined;opener=undefined;}
 return {
  begin(command,snapshot){
   if(!available()||command.generation!==snapshot.generation)return;
   if(!['workspace-activity','close-workspace-activity','history','workspace','select','strip'].includes(command.kind))return;
   const workspace=command.workspace??command.route?.workspace;
   if(['workspace-activity','history'].includes(command.kind)&&workspace!==snapshot.activeWorkspace)return;
   const element=document.activeElement;
   return {generation:command.generation,workspace,kind:command.kind,view:command.view,
    element:element!==document.body&&!surface.contains(element)&&visible(element)?element:undefined};
  },
  accepted(request){
   if(closed||!request)return;
   if(request.kind==='close-workspace-activity'){
    if(opener&&opener.generation===request.generation&&opener.workspace===request.workspace)opener.dismissalControl=request.element;
    pending=undefined;return;
   }
   // A newer successful navigation supersedes the prior opener immediately,
   // including before its queued transition appears in a snapshot.
   opener=undefined;
   pending=['workspace-activity','history'].includes(request.kind)?request:undefined;
  },
  errors(errors){if(errors.length>errorCount)pending=undefined;errorCount=errors.length;},
  failed(request){if(!request||pending===request)pending=undefined;},
  update(snapshot){
   if(!available()){clear();return;}
   if(pending&&!matches(pending,snapshot))pending=undefined;
   if(opener&&!matches(opener,snapshot))opener=undefined;
   const pane=snapshot.workspaceActivity;
   if(pending){
    const opened=pending.kind==='workspace-activity'&&pane?.workspace===pending.workspace&&pane.view===pending.view;
    const documentOpened=pending.kind==='history'&&!pane;
    if(opened||documentOpened){
     const request=pending;pending=undefined;
     opener=opened?request:undefined;
     // Only an explicit external invocation hands focus to this mount.
     if(request.element&&document.activeElement===request.element)canvas.focus({preventScroll:true});
    }
   }
   if(opener&&!pane){
    const request=opener;opener=undefined;
    if(snapshot.workspaceActivityCloseReason==='dismissed'&&visible(request.element)
      &&(surface.contains(document.activeElement)||document.activeElement===document.body||document.activeElement===request.dismissalControl))request.element.focus({preventScroll:true});
   }
  },
  close(){closed=true;clear();},
 };
}
