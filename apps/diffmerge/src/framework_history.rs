//! Frozen merged output. The unsaved external originals cannot be reconstructed.
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::history_mode::adapter::{map_collection, navigate_pages};
use ui::history_mode::restore::{HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType, ScalarType};
use ui::history_mode::{HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition, PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView};
use ui::{TextRenderer, Theme};
use vello::{Scene, kurbo::{Affine, Rect}, peniko::{BlendMode, Fill}};
use crate::doc::MergeDoc;
pub static HISTORY: HistoryDefinition = HistoryDefinition { app_id: "dev.qos.diffmerge", scopes: &[HistoryScopeDefinition {
    key: "output", title: "Merge output", presentation: HistoryPresentation::Domain { create: preview }, content_schema: Some(&SCHEMA),
}] };
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema { id: "diffmerge.output", version: 1,
    roots: &[
        HistoryRestoreRoot { key:"merged", required:false, expected_type:RestoreValueType::Object(ObjType::Text) },
        HistoryRestoreRoot { key:"resolutions", required:false, expected_type:RestoreValueType::Object(ObjType::Map) },
        HistoryRestoreRoot { key:"session", required:false, expected_type:RestoreValueType::Scalar(ScalarType::String) },
    ], selectors:&[], preserved_roots:&[], preserved:&[], validate, validate_result:validate,
};
fn validate(doc:&AutoCommit)->Result<(),String> {
    if let Some((value,_))=doc.get(ROOT,"merged").map_err(|e|e.to_string())? { if value!=Value::Object(ObjType::Text) { return Err("Merged output must be Text".into()); } }
    if let Some((value,_))=doc.get(ROOT,"session").map_err(|e|e.to_string())? { if value.to_str().is_none() { return Err("Session must be a string".into()); } }
    if let Some(map)=map_collection(doc,"resolutions")? {
        for key in doc.keys(&map) {
            let index=key.parse::<usize>().map_err(|_|"Invalid hunk index")?;
            if index.to_string()!=key { return Err("Noncanonical hunk index".into()); }
            match doc.get(&map,&key).map_err(|e|e.to_string())? {
                Some((Value::Scalar(v),_)) if matches!(v.as_ref(),ScalarValue::Str(s) if ["ours","theirs","both"].contains(&s.as_str())) => {},
                _=>return Err("Invalid recorded hunk choice".into()),
            }
        }
    }
    Ok(())
}
fn preview(input:PreviewInput)->Result<Box<dyn HistoryPreview>,String> {
    let d=AutoCommit::load(input.snapshot.bytes()).map_err(|e|e.to_string())?; validate(&d)?;
    let mut view=input.initial_view;
    if !["output","originals"].contains(&view.page.as_deref().unwrap_or_default()) { view.page=Some("output".into()); }
    Ok(Box::new(OutputPreview { output:MergeDoc::merged_text(&d), recorded:MergeDoc::has_merged(&d), resolutions:MergeDoc::resolutions(&d), session:MergeDoc::session(&d), view, bounds:Rect::ZERO }))
}
struct OutputPreview { output:String, recorded:bool, resolutions:Vec<(usize,String)>, session:Option<String>, view:PreviewView, bounds:Rect }
impl HistoryPreview for OutputPreview {
    fn layout(&mut self,bounds:Rect,_:&mut TextRenderer,_:&Theme)->Result<(),String>{self.bounds=bounds;Ok(())}
    fn draw(&mut self,scene:&mut Scene,t:Affine,tr:&mut TextRenderer,theme:&Theme){
        scene.fill(Fill::NonZero,t,theme.bg_primary,None,&self.bounds);
        scene.push_layer(Fill::NonZero,BlendMode::default(),1.,t,&self.bounds);
        if self.view.page.as_deref()==Some("output") && self.recorded {
            let x=self.bounds.x0+16.-self.view.scroll.0;
            let (_,h)=tr.measure("M",14.,None); let pitch=f64::from(h).max(20.);
            for (i,line) in self.output.lines().enumerate() {
                let y=self.bounds.y0+16.+i as f64*pitch-self.view.scroll.1;
                if y+pitch<self.bounds.y0 {continue;} if y>self.bounds.y1 {break;}
                tr.draw_text(scene,&(i+1).to_string(),12.,theme.text_dim,t*Affine::translate((x,y)),None);
                crate::merge_view::draw_code_segments(scene,t,tr,theme,x+48.,y,line);
            }
        } else {
            tr.draw_text(scene,&self.accessibility().description,15.,theme.text_dim,t*Affine::translate((self.bounds.x0+16.,self.bounds.y0+16.-self.view.scroll.1)),Some(self.bounds.width() as f32-32.));
        }
        scene.pop_layer();
    }
    fn navigate(&mut self,c:PreviewNavigation)->bool {let pages=self.accessibility().pages;navigate_pages(&mut self.view,c,&pages)}
    fn view(&self)->PreviewView {self.view.clone()}
    fn accessibility(&self)->PreviewAccessibility {
        let description=if self.view.page.as_deref()==Some("originals") {
            let mut s="Original inputs unavailable: base, ours and theirs were not recorded. Recorded choices are metadata only; historical hunk identity cannot be verified.".to_string();
            if let Some(label)=&self.session {s.push_str(&format!("\nSession label: {label}"));}
            for (h,choice) in &self.resolutions {s.push_str(&format!("\nRecorded hunk {h}: {choice}"));} s
        } else if self.recorded {format!("Merged output\n{}",self.output)} else {"No merged output recorded in this version".into()};
        PreviewAccessibility{description,pages:vec![("output".into(),"Merged output".into()),("originals".into(),"Original inputs unavailable / recorded choices".into())]}
    }
}
