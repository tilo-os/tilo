use std::{collections::HashMap, path::Path};
use notes::{NotesApp, model::Workspace, storage, num};
use ui::{taffy, vello, winit};
use taffy::prelude::*;
use ui::{widget::{Widget, marks::Mark}, text_render::TextRenderer, theme::Theme, presence::PresenceCtx,
    permissions::PermissionContext, offscreen::OffscreenRenderer};
use vello::{Scene, kurbo::{Affine, Point}, peniko::Color};
use winit::event::{WindowEvent, ElementState, MouseButton, Modifiers};
fn settle_layout(
    app: &mut NotesApp,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    w: u32,
    h: u32,
) -> Result<(TaffyTree<()>, NodeId), Box<dyn std::error::Error>> {
    let mut kept: Option<(TaffyTree<()>, NodeId)> = None;
    for _ in 0..5 {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = app.build_layout(&mut taffy, theme, &mut node_map);
        let root_node = taffy.new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(num::f32_of_u32(w)),
                    height: Dimension::length(num::f32_of_u32(h)),
                },
                ..Default::default()
            },
            &[child],
        )?;
        {
            let measure_map = node_map.clone();
            let tr = &mut *text_renderer;
            taffy.compute_layout_with_measure(
                root_node,
                taffy::Size {
                    width: AvailableSpace::Definite(num::f32_of_u32(w)),
                    height: AvailableSpace::Definite(num::f32_of_u32(h)),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, tr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )?;
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);
        let settled = !app.take_structure_dirty();
        kept = Some((taffy, child));
        if settled {
            break;
        }
    }
    let Some(result) = kept else {
        return Err("layout cycle never ran".into());
    };
    Ok(result)
}


fn paint(app: &mut NotesApp, tr: &mut TextRenderer, theme: &Theme, path: &Path) {
    let mut scene=Scene::new();
    app.draw_with_presence(&mut scene, Affine::IDENTITY, tr, theme, &PresenceCtx::empty(), &PermissionContext::permissive());
    OffscreenRenderer::new(1200,800).render_png(&scene, Color::from_rgba8(15,17,21,255), path);
}
fn send(app: &mut NotesApp, state: &mut ui::State, tree: &(TaffyTree<()>, NodeId), event: WindowEvent) {
    app.handle_event(&event,1.0,&Modifiers::default(),&mut state.doc,&state.root,&mut false,&mut None,tree.1,&tree.0);
}
fn pointer(x:f64,y:f64)->WindowEvent {
    WindowEvent::CursorMoved { device_id: unsafe {std::mem::zeroed()}, position: winit::dpi::PhysicalPosition::new(x,y) }
}
fn mouse(state:ElementState)->WindowEvent {
    WindowEvent::MouseInput {device_id: unsafe {std::mem::zeroed()}, state, button:MouseButton::Left}
}
fn main() {
    let out=std::path::PathBuf::from(std::env::args().nth(1).expect("output directory"));
    std::fs::create_dir_all(&out).unwrap();
    for scale in [1.0,2.0] {
        let root=out.join(format!("workspace-{scale}"));
        storage::ensure_layout(&root).unwrap();
        let mut ws=Workspace::new();
        let source=ws.create_page(None); ws.rename(source,"Rich mark interaction");
        let target=ws.create_page(None); ws.rename(target,"Reading list");
        ws.set_selected(Some(source)); storage::save_workspace(&root,&ws).unwrap();
        storage::write_page_body(&root,source,"Owner in **bold** and `code`.\n\nSee [[Reading list]] for sources.").unwrap();
        storage::write_page_body(&root,target,"The link opened this page.").unwrap();
        let mut app=NotesApp::spawn(root).unwrap();
        let mut state=ui::State::new();
        app.bypass_seed_grace_for_test();
        app.sync(&mut state.doc,&state.root);
        app.navigate_to(target); app.sync(&mut state.doc,&state.root);
        app.navigate_to(source); app.sync(&mut state.doc,&state.root);
        let theme=Theme::dark().with_text_scale(scale);
        let mut tr=TextRenderer::new(); tr.set_text_scale(scale);
        let tree=settle_layout(&mut app,&theme,&mut tr,1200,800).unwrap();
        paint(&mut app,&mut tr,&theme,&out.join(format!("initial-{scale}x.png")));
        let (left,right,y,range)={
            let line=&app.main_panel_mut().editor().blocks()[0].line;
            let mark=line.marks().iter().find(|m|m.mark==Mark::Bold).unwrap();
            (line.pixel_x_for_byte(&theme,mark.range.start),line.pixel_x_for_byte(&theme,mark.range.end),line.get_rect().center().y,mark.range.clone())
        };
        send(&mut app,&mut state,&tree,pointer(right,y));
        send(&mut app,&mut state,&tree,mouse(ElementState::Pressed));
        send(&mut app,&mut state,&tree,mouse(ElementState::Released));
        assert_eq!(app.main_panel_mut().editor().blocks()[0].line.cursor_pos(),range.end,"caret at painted bold end");
        paint(&mut app,&mut tr,&theme,&out.join(format!("caret-{scale}x.png")));
        send(&mut app,&mut state,&tree,pointer(left,y));
        send(&mut app,&mut state,&tree,mouse(ElementState::Pressed));
        send(&mut app,&mut state,&tree,pointer(right,y));
        send(&mut app,&mut state,&tree,mouse(ElementState::Released));
        assert_eq!(app.main_panel_mut().editor().blocks()[0].line.selection_range(),Some((range.start,range.end)),"actual drag selects exactly bold");
        paint(&mut app,&mut tr,&theme,&out.join(format!("selection-{scale}x.png")));
        let (code_left,code_right,code_range)={
            let line=&app.main_panel_mut().editor().blocks()[0].line;
            let mark=line.marks().iter().find(|m|m.mark==Mark::Code).unwrap();
            (line.pixel_x_for_byte(&theme,mark.range.start),line.pixel_x_for_byte(&theme,mark.range.end),mark.range.clone())
        };
        send(&mut app,&mut state,&tree,pointer(code_left,y));
        send(&mut app,&mut state,&tree,mouse(ElementState::Pressed));
        send(&mut app,&mut state,&tree,pointer(code_right,y));
        send(&mut app,&mut state,&tree,mouse(ElementState::Released));
        assert_eq!(app.main_panel_mut().editor().blocks()[0].line.selection_range(),Some((code_range.start,code_range.end)));
        paint(&mut app,&mut tr,&theme,&out.join(format!("code-selection-{scale}x.png")));
        let link_index=app.main_panel_mut().editor().blocks().iter().position(|b|b.line.marks().iter().any(|m|matches!(m.mark,Mark::Link(_)))).unwrap();
        let link_range=app.main_panel_mut().editor().blocks()[link_index].line.marks().iter().find(|m|matches!(m.mark,Mark::Link(_))).unwrap().range.clone();
        let block_id=app.main_panel_mut().editor().blocks()[link_index].id.clone();
        let marks=[ui::widget::marks::MarkSpan::new(link_range.clone(),Mark::Link(target)),
            ui::widget::marks::MarkSpan::new(link_range.clone(),Mark::Bold)];
        let action=notes::actions::set_block_marks_action(source,block_id,marks.iter().map(notes::actions::InlineMarkSpan::from_ui).collect());
        ui::ActionHost::new().try_dispatch(&mut state.doc,notes::actions::NOTES_ACTION_TYPE,&action).unwrap();
        app.sync(&mut state.doc,&state.root);
        paint(&mut app,&mut tr,&theme,&out.join(format!("bold-link-{scale}x.png")));
        let (edge,link_y)={ let line=&app.main_panel_mut().editor().blocks()[link_index].line;
            (line.pixel_x_for_byte(&theme,link_range.end),line.get_rect().center().y) };
        println!("BEFORE link target={target} doc-target={} hit={:?} edge={edge} y={link_y}", notes::doc::NotesDoc::page_blocks_list(&state.doc,target).is_some(), app.main_panel_mut().editor().link_at_point(Point::new(edge-0.1,link_y)));
        assert_eq!(app.main_panel_mut().editor().link_at_point(Point::new(edge-0.1,link_y)),Some(target));
        assert_eq!(app.main_panel_mut().editor().link_at_point(Point::new(edge+0.1,link_y)),None);
        send(&mut app,&mut state,&tree,pointer(edge-0.1,link_y));
        send(&mut app,&mut state,&tree,mouse(ElementState::Pressed));
        send(&mut app,&mut state,&tree,mouse(ElementState::Released));
        assert_eq!(app.selected_for_test(),Some(target),"real app click at final bold-link edge navigates");
        settle_layout(&mut app,&theme,&mut tr,1200,800).unwrap();
        paint(&mut app,&mut tr,&theme,&out.join(format!("link-opened-{scale}x.png")));
        println!("PASS {scale}x caret={} selection={:?} code-selection={:?} link-edge={edge:.3} selected={target}",range.end,range,code_range);
    }
}
