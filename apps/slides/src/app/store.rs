//! The slides editor as a bound view: the [`Store`] the editor shares with
//! the view's effect handler, and [`window`], which takes the editor into
//! the view.
//!
//! The effects answer here, from the deck the document holds; what they ask
//! of the editor itself (a mode, a slide or element to show, a picture to
//! place, a toast) waits in [`Store::asks`] until the editor next syncs.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_content::ContentStore;
use ui::framework::{EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_resource::{ActionDecl, Id};
use ui::widget::toast::ToastKind;

use super::{
    DeckRecord, SlidesApp, image_open_request, is_pptx, load_presentation_from_path,
    open_document_request, parse_presentation, sanitize_filename, save_document_request,
    save_presentation_to_path,
};
use crate::actions;
use crate::effects::SlidesEffect;
use crate::model::{Deck, ElementKind};
use crate::strings::tr;

/// What an effect asked of the editor, done when it next syncs.
pub(crate) enum Ask {
    /// Show what a view effect names: a mode, a panel, a slide, an element
    /// or its notes. The effect was checked against the document.
    Show(SlidesEffect),
    /// Place the picture stored as this `src` on the current slide.
    InsertPicture(String),
    /// Show a deck just opened from a file from its first slide.
    ShowOpenedDeck,
    /// Say something in the window's bar.
    Say(String, ToastKind),
    /// Stage a capture's scenario (`None` for a plain capture).
    Stage(Option<String>),
}

/// The element selected in the editor, as the edits bound to it name it.
pub(crate) struct Selected {
    pub(crate) slide: Id,
    pub(crate) element: Id,
    pub(crate) label: String,
}

/// What the editor and the view's effect handler share.
pub(crate) struct Store {
    /// Where pictures' and presentations' bytes live, addressed by their
    /// hash; the editor's image decoder reads the same store.
    pub(super) content: ContentStore,
    /// The element selected in the editor, while it edits one.
    pub(super) selected: Option<Selected>,
    /// What effects asked of the editor since it last synced.
    pub(super) asks: Vec<Ask>,
}

/// The [`Store`] as the editor and the view share it.
pub(crate) type Shared = Rc<RefCell<Store>>;

impl Store {
    pub(super) fn new(content: ContentStore) -> Self {
        Self {
            content,
            selected: None,
            asks: Vec::new(),
        }
    }

    pub(super) fn ask(&mut self, ask: Ask) {
        self.asks.push(ask);
    }

    /// Show `message` as a failure in the window's bar, and answer it.
    fn fail<T>(&mut self, message: String) -> Result<T, String> {
        self.ask(Ask::Say(message.clone(), ToastKind::Error));
        Err(message)
    }

    /// Answer one of the editor's effects, from a control, a key, the
    /// palette or an agent.
    fn answer(
        &mut self,
        effect: SlidesEffect,
        me: &Shared,
        cx: &mut EffectCx<DeckRecord>,
    ) -> EffectAnswer {
        let deck = || crate::deck::mirror(&cx.live().get());
        match effect {
            SlidesEffect::OpenSlide { ref slide } | SlidesEffect::OpenNotes { ref slide } => {
                slide_index(&deck(), slide)?;
            }
            SlidesEffect::OpenElement {
                ref slide,
                ref element,
            } => {
                element_kind(&deck(), slide, element)?;
            }
            SlidesEffect::EditText {
                ref slide,
                ref element,
            } => {
                if element_kind(&deck(), slide, element)? != ElementKind::Text {
                    return Err("element is not editable text".into());
                }
            }
            SlidesEffect::InsertPicture => return Ok(choose_picture(me, cx)),
            SlidesEffect::OpenPresentation { path } => {
                return Ok(self.open_presentation(path, me, cx));
            }
            SlidesEffect::SaveAs { path } => return self.save_presentation(path, me, cx),
            SlidesEffect::ReadDeck => {
                return Ok(Effected::value(crate::outline::of_deck(&deck())));
            }
            SlidesEffect::ImportPptxContent { content } => {
                return self.import_pptx_content(&content, cx);
            }
            SlidesEffect::ExportPptxContent => return self.export_pptx_content(cx),
            SlidesEffect::Editor
            | SlidesEffect::Overview
            | SlidesEffect::Present
            | SlidesEffect::PresentLargestExternal
            | SlidesEffect::PresentPrimaryExternal
            | SlidesEffect::EndPresentation
            | SlidesEffect::ShowSlideList { .. }
            | SlidesEffect::ShowProperties { .. } => {}
        }
        self.ask(Ask::Show(effect));
        Ok(Effected::done())
    }

    /// Open a presentation file in place of the deck: the file at `path`,
    /// or the one the person chooses with the host's chooser. It is read off
    /// the UI thread and lands as one action, one undo step.
    fn open_presentation(
        &self,
        path: Option<ui::tilo_resource::AbsolutePath>,
        me: &Shared,
        cx: &mut EffectCx<DeckRecord>,
    ) -> Effected {
        let content = self.content.clone();
        let me = Rc::clone(me);
        match path {
            Some(path) => cx.thread(
                move |_| load_presentation_from_path(&path, &content),
                move |(), loaded, cx| me.borrow_mut().finish_open(loaded, cx),
            ),
            None => cx.choose_file(open_document_request(), move |(), chosen, cx| {
                let Some(file) = chosen else {
                    return Ok(Effected::done());
                };
                let pptx = is_pptx(std::path::Path::new(&file.name));
                if pptx {
                    me.borrow_mut()
                        .ask(Ask::Say(tr("slides-toast-opening-pptx"), ToastKind::Info));
                }
                Ok(cx.thread(
                    move |_| parse_presentation(&file.bytes, pptx, &content),
                    move |(), loaded, cx| me.borrow_mut().finish_open(loaded, cx),
                ))
            }),
        }
    }

    /// Show the outcome of opening a presentation, replacing the deck when
    /// it was read.
    fn finish_open(
        &mut self,
        loaded: Result<Deck, String>,
        cx: &EffectCx<DeckRecord>,
    ) -> EffectAnswer {
        match loaded {
            Ok(deck) => {
                cx.apply(&actions::replace_deck_action(&deck));
                self.ask(Ask::ShowOpenedDeck);
                Ok(Effected::done())
            }
            Err(error) => {
                self.ask(Ask::Say(
                    tr("slides-toast-process-failed"),
                    ToastKind::Error,
                ));
                Err(error)
            }
        }
    }

    /// Save the deck as a presentation file: at `path` (PPTX by its
    /// extension, else ODP), or as ODP where the person chooses with the
    /// host's chooser. It is written off the UI thread.
    fn save_presentation(
        &mut self,
        path: Option<ui::tilo_resource::AbsolutePath>,
        me: &Shared,
        cx: &mut EffectCx<DeckRecord>,
    ) -> EffectAnswer {
        let deck = crate::deck::mirror(&cx.live().get());
        if !actions::deck_within_aggregate_limits(&deck) {
            return self.fail(tr("slides-toast-export-limit"));
        }
        let content = self.content.clone();
        let me = Rc::clone(me);
        Ok(match path {
            Some(path) => {
                cx.thread(
                    move |_| save_presentation_to_path(&deck, &path, &content),
                    // Saving where the deck lives says nothing; only a
                    // failure is news.
                    move |(), saved, _| me.borrow_mut().finish_save(saved.map(|()| true)),
                )
            }
            None => {
                let name = format!("{}.odp", sanitize_filename(&deck.title));
                cx.thread(
                    move |_| crate::odp::write_odp(&deck, &content).map_err(|e| e.to_string()),
                    move |(), bytes, cx| match bytes {
                        Ok(bytes) => Ok(cx.save_file(
                            save_document_request(&name),
                            bytes,
                            move |(), saved, _| me.borrow_mut().finish_save(Ok(saved)),
                        )),
                        Err(error) => me.borrow_mut().finish_save(Err(error)),
                    },
                )
            }
        })
    }

    /// Show a failed save. A save through the chooser has nothing more to
    /// say: the person just picked the file.
    fn finish_save(&mut self, saved: Result<bool, String>) -> EffectAnswer {
        match saved {
            Ok(_) => Ok(Effected::done()),
            Err(error) => {
                self.ask(Ask::Say(
                    "Could not process the presentation".into(),
                    ToastKind::Error,
                ));
                Err(error)
            }
        }
    }
}

/// Ask the person for a picture with the host's chooser (the app never
/// draws its own picker), then keep its bytes in the content store and
/// place it on the current slide. A cancelled choice places nothing.
fn choose_picture(me: &Shared, cx: &mut EffectCx<DeckRecord>) -> Effected {
    let me = Rc::clone(me);
    cx.choose_file(image_open_request(), move |(), chosen, _| {
        let Some(file) = chosen else {
            return Ok(Effected::done());
        };
        let mut store = me.borrow_mut();
        let Some(src) = crate::assets::store_asset(&store.content, &file.bytes) else {
            return store.fail(tr("slides-toast-image-import-failed"));
        };
        store.ask(Ask::InsertPicture(src));
        Ok(Effected::done())
    })
}

fn slide_index(deck: &Deck, slide: &str) -> Result<usize, String> {
    deck.index_of(slide)
        .ok_or_else(|| "slide no longer exists".to_string())
}

/// What kind of element `element` of slide `slide` is.
fn element_kind(deck: &Deck, slide: &str, element: &str) -> Result<ElementKind, String> {
    let index = slide_index(deck, slide)?;
    deck.slides
        .get(index)
        .and_then(|slide| slide.element(element))
        .map(|element| element.kind)
        .ok_or_else(|| "element no longer exists".into())
}

/// The edits bound to the selected element, published on the deck beside
/// its declared actions: delete it, bring it to the front, send it to the
/// back.
pub(crate) fn selection_declarations(selected: Option<&Selected>) -> Vec<ActionDecl<DeckRecord>> {
    let Some(Selected {
        slide,
        element,
        label,
    }) = selected
    else {
        return Vec::new();
    };
    let mut declarations = Vec::new();
    if let Some(delete) = DeckRecord::DELETE_ELEMENT
        .bind(
            crate::deck::deck::DeleteElement::paths().slide,
            slide.clone(),
        )
        .and(
            crate::deck::deck::DeleteElement::paths().element,
            element.clone(),
        )
        .declaration("delete-selection", format!("Delete {label}"))
    {
        declarations.push(delete.describe("Delete the element selected in the editor."));
    }
    let front = DeckRecord::BRING_TO_FRONT
        .bind(
            crate::deck::deck::BringToFront::paths().slide,
            slide.clone(),
        )
        .and(
            crate::deck::deck::BringToFront::paths().element,
            element.clone(),
        )
        .declaration("bring-selection-front", format!("Bring to front: {label}"));
    let back = DeckRecord::SEND_TO_BACK
        .bind(crate::deck::deck::SendToBack::paths().slide, slide.clone())
        .and(
            crate::deck::deck::SendToBack::paths().element,
            element.clone(),
        )
        .declaration("send-selection-back", format!("Send to back: {label}"));
    for restack in [front, back].into_iter().flatten() {
        declarations.push(restack.describe("Restack the element selected in the editor."));
    }
    declarations
}

/// The view of the deck: `editor` follows the document, its [`Store`]
/// answers the effects, and the view binds the edits of the selected
/// element beside it.
pub(crate) fn window(cx: &ViewCx<DeckRecord>, mut editor: SlidesApp) -> ui::widget::Element {
    editor.live = Some(cx.live().clone());
    cx.on_open_item(editor.opener());
    let store = Rc::clone(&editor.store);
    {
        let answer = Rc::clone(&store);
        cx.effects(move |effect: SlidesEffect, cx| answer.borrow_mut().answer(effect, &answer, cx));
    }
    {
        let store = Rc::clone(&store);
        cx.on_stage(move |scenario| {
            store
                .borrow_mut()
                .ask(Ask::Stage(scenario.map(str::to_owned)));
        });
    }
    cx.bind_declarations(move |_| selection_declarations(store.borrow().selected.as_ref()));
    Box::new(editor)
}
