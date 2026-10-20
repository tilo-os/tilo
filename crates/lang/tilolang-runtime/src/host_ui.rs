//! Imports that build a view's UI tree. Views only.

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_void, trap};
use crate::engine::Wasm;
use crate::handles::{FrameKind, ViewState};
use crate::value::Value;
use crate::view::{Binding, NodeBuild};

impl Core {
    fn with_view<R>(&self, f: impl FnOnce(&mut ViewState) -> Result<R, String>) -> HResult<R> {
        let mut frames = self.frames.borrow_mut();
        match &mut frames.top_mut()?.kind {
            FrameKind::View(vs) => Ok(f(vs)?),
            _ => trap("ui nodes can only be built inside a view"),
        }
    }

    fn node_index(&self, h: i32) -> HResult<usize> {
        self.with_val(h, |v| match v {
            Value::Node(i) => Ok(*i),
            other => trap(format!("expected a ui node, got {}", other.kind_name())),
        })
    }

    fn set_prop(&self, node: i32, name: i32, v: Value) -> HResult<Option<Wasm>> {
        let (at, name) = (self.node_index(node)?, self.text(name)?);
        if matches!(v, Value::Node(_) | Value::Rec(_)) {
            return trap("a ui node or record cannot be a property; use ui_child");
        }
        self.with_view(|vs| {
            let n = vs.nodes.get_mut(at).ok_or("unknown ui node")?;
            n.props.insert(name, v);
            Ok(())
        })?;
        ret_void()
    }

    pub(crate) fn ui_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::UiNode => {
                let (kind, key) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                let at = self.with_view(|vs| {
                    vs.nodes.push(NodeBuild {
                        kind,
                        key,
                        props: Default::default(),
                        children: Vec::new(),
                        binding: None,
                        events: Default::default(),
                        anchor: None,
                    });
                    Ok(vs.nodes.len().saturating_sub(1))
                })?;
                self.ret_h(Value::Node(at))
            }
            Imp::UiPropH => {
                let v = self.val(a.h(2)?)?;
                self.set_prop(a.h(0)?, a.h(1)?, v)
            }
            Imp::UiPropI => self.set_prop(a.h(0)?, a.h(1)?, Value::Int(a.long(2)?)),
            Imp::UiPropF => self.set_prop(a.h(0)?, a.h(1)?, Value::Float(a.float(2)?)),
            Imp::UiChild => {
                let (parent, child) = (self.node_index(a.h(0)?)?, self.node_index(a.h(1)?)?);
                self.with_view(|vs| {
                    if vs.nodes.get(child).is_none() {
                        return Err("unknown ui node".to_owned());
                    }
                    let p = vs.nodes.get_mut(parent).ok_or("unknown ui node")?;
                    p.children.push(child);
                    Ok(())
                })?;
                ret_void()
            }
            Imp::UiBind => {
                let node = self.node_index(a.h(0)?)?;
                let r = self.ref_arg(a.h(1)?)?;
                let field = self.text(a.h(2)?)?;
                let (_, f) = self.lookup_field(&r.target, &field)?;
                let binding = Binding {
                    target: r.target,
                    field: f.id,
                };
                self.with_view(|vs| {
                    vs.nodes.get_mut(node).ok_or("unknown ui node")?.binding = Some(binding);
                    Ok(())
                })?;
                ret_void()
            }
            Imp::UiOn => {
                let node = self.node_index(a.h(0)?)?;
                let event = self.text(a.h(1)?)?;
                let act = match self.val(a.h(2)?)? {
                    v @ Value::Action(_) => v,
                    other => {
                        return trap(format!(
                            "expected an action reference, got {}",
                            other.kind_name()
                        ));
                    }
                };
                self.with_view(|vs| {
                    vs.nodes
                        .get_mut(node)
                        .ok_or("unknown ui node")?
                        .events
                        .insert(event, act);
                    Ok(())
                })?;
                ret_void()
            }
            Imp::UiAnchor => {
                let node = self.node_index(a.h(0)?)?;
                let anchor = self.text(a.h(1)?)?;
                self.with_view(|vs| {
                    vs.nodes.get_mut(node).ok_or("unknown ui node")?.anchor = Some(anchor);
                    Ok(())
                })?;
                ret_void()
            }
            _ => trap("not a ui import"),
        }
    }
}
