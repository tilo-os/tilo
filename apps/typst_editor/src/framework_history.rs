//! Immutable source and embedded-font compilation; no library, worker or IO.
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use ui::history_mode::adapter::navigate_pages;
use ui::history_mode::restore::{HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType};
use ui::history_mode::{HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition, PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView};
use ui::{TextRenderer, Theme};
use vello::{Scene, kurbo::{Affine, Rect}};
pub static HISTORY: HistoryDefinition = HistoryDefinition { app_id:"dev.qos.typst-editor", scopes:&[HistoryScopeDefinition {
    key:"source",title:"Typst source",presentation:HistoryPresentation::Domain{create:preview},content_schema:Some(&SCHEMA),
}] };
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema { id:"typst-editor.source",version:1,
    roots:&[HistoryRestoreRoot{key:"source",required:true,expected_type:RestoreValueType::Object(ObjType::Text)}],selectors:&[],preserved_roots:&[],preserved:&[],validate,validate_result:validate,
};
fn validate(doc:&AutoCommit)->Result<(),String>{
    match doc.get(ROOT,"source").map_err(|e|e.to_string())? {None|Some((Value::Object(ObjType::Text),_))=>Ok(()),_=>Err("Recorded source must be Text".into())}
}
fn preview(input:PreviewInput)->Result<Box<dyn HistoryPreview>,String>{
    let d=AutoCommit::load(input.snapshot.bytes()).map_err(|e|e.to_string())?;validate(&d)?;
    let source=d.get(ROOT,"source").map_err(|e|e.to_string())?.map(|(_,id)|d.text(&id).map_err(|e|e.to_string())).transpose()?;
    let mut editor=crate::editor::SourceEditor::new();editor.focused=false;editor.set_text(source.as_deref().unwrap_or_default());
    let mut pane=crate::preview::PreviewPane::new();
    let output=source.as_ref().map(|s|crate::compile::compile_source(&crate::compile::WorldParts::new(),s,1.));
    let mut diagnostics=crate::diagnostics::DiagnosticsPanel::new();
    let mut pages=vec![("source".into(),"Recorded source".into())];
    let mut compiled_text=Vec::new();
    if let Some(out)=output {
        pane.set_pages_with_text(&out.pages,&out.page_text);
        for (i,runs) in out.page_text.iter().enumerate(){
            pages.push((format!("page:{i}"),format!("Compiled page {}",i+1)));
            compiled_text.push(runs.iter().flat_map(|r|r.cells.iter().map(|c|c.text.as_str())).collect::<String>());
        }
        if !out.diagnostics.is_empty(){pages.push(("diagnostics".into(),"Compilation diagnostics".into()));}
        diagnostics.set_diagnostics(out.diagnostics);
    }
    let mut view=input.initial_view;
    if !pages.iter().any(|(id,_)|Some(id)==view.page.as_ref()){view.page=Some("source".into());}
    Ok(Box::new(TypstPreview{source,editor,pane,diagnostics,pages,compiled_text,view,bounds:Rect::ZERO}))
}
struct TypstPreview{source:Option<String>,editor:crate::editor::SourceEditor,pane:crate::preview::PreviewPane,diagnostics:crate::diagnostics::DiagnosticsPanel,pages:Vec<(String,String)>,compiled_text:Vec<String>,view:PreviewView,bounds:Rect}
impl TypstPreview{
    fn page_index(&self)->Option<usize>{self.view.page.as_deref()?.strip_prefix("page:")?.parse().ok()}
}
impl HistoryPreview for TypstPreview{
    fn layout(&mut self,bounds:Rect,_:&mut TextRenderer,_:&Theme)->Result<(),String>{
        self.bounds=bounds;self.editor.set_rect(bounds);self.pane.set_rect(bounds);self.diagnostics.set_rect(bounds);
        if let Some(page)=self.page_index(){self.pane.set_snapshot_view(page,self.view.zoom,self.view.scroll);}
        Ok(())
    }
    fn draw(&mut self,s:&mut Scene,t:Affine,tr:&mut TextRenderer,theme:&Theme){
        if self.source.is_none(){tr.draw_text(s,"No source recorded in this version. Restore is unavailable.",16.,theme.text_dim,t*Affine::translate((self.bounds.x0+16.,self.bounds.y0+16.)),Some(self.bounds.width() as f32-32.));}
        else if self.view.page.as_deref()==Some("diagnostics"){self.diagnostics.draw(s,t,tr,theme);}
        else if self.page_index().is_some(){self.pane.draw(s,t,tr,theme);}
        else{self.editor.draw(s,t,tr,theme,self.diagnostics.diags());}
    }
    fn navigate(&mut self,c:PreviewNavigation)->bool{
        if let PreviewNavigation::Zoom{factor,anchor}=c {
            if self.page_index().is_none() || !factor.is_finite() || factor<=0. || !anchor.x.is_finite() || !anchor.y.is_finite(){return false;}
            self.view.zoom=(self.view.zoom*factor).clamp(0.25,4.);return true;
        }
        let old=self.view.clone();
        if !navigate_pages(&mut self.view,c,&self.pages){return false;}
        if self.view.page!=old.page {self.editor.reset_cursor();self.diagnostics.scroll_by(-100_000.);}
        let dy=self.view.scroll.1-old.scroll.1;
        if self.view.page.as_deref()==Some("source"){self.editor.scroll_by(0.,dy as f32,self.source.as_deref().unwrap_or_default());}
        if self.view.page.as_deref()==Some("diagnostics"){self.diagnostics.scroll_by(dy);}
        true
    }
    fn view(&self)->PreviewView{self.view.clone()}
    fn accessibility(&self)->PreviewAccessibility{
        let description=if let Some(source)=&self.source{
            if let Some(i)=self.page_index(){format!("Compiled page {}\n{}",i+1,self.compiled_text.get(i).map_or("",String::as_str))}
            else if self.view.page.as_deref()==Some("diagnostics"){self.diagnostics.diags().iter().map(|d|format!("Line {}: {}",d.line.unwrap_or(0),d.message)).collect::<Vec<_>>().join("\n")}
            else {let mut s=format!("Recorded source\n{source}");if self.diagnostics.error_count()>0{s.push_str("\nCompilation failed; diagnostics available. No current or last-good pages are shown.");}s}
        }else{"No source recorded in this version. Restore is unavailable.".into()};
        PreviewAccessibility{description,pages:self.pages.clone()}
    }
}
