//! A data-driven egui renderer for [`gantz_ui`] element trees.
//!
//! [`UiTree`] walks a decoded [`Element`] tree each frame, resolving widget
//! bindings against VM state and reporting interactions as response
//! payloads. It is the single rendering path for widget node GUIs: builtins
//! construct their own fragments and render through it, and graph-defined
//! GUIs evaluate to trees rendered the same way.
//!
//! ## Bindings
//!
//! A widget's `bind` path is relative to the graph its tree was defined in.
//! The walker resolves it as `instance prefix ++ scope prefixes ++ bind
//! path` and reads via [`NodeCtx::extract_value_at`], writes via
//! [`NodeCtx::update_value_at`]. Those touch VM runtime state only, so
//! interpreting a tree never mutates CA-affecting state and
//! [`UiTreeResponse`] carries no `changed` flag.
//!
//! ## Identity
//!
//! Every element's [`egui::Id`] derives from the render root's id, its
//! structural position beneath its parent (overridden by a `key` attr), and
//! any enclosing `scope` ids. Label text never contributes, so relabelling
//! never resets widget state.
//!
//! ## Errors
//!
//! [`gantz_ui::decode()`] is total: every node of the tree is renderable and
//! [`Element::Error`] is the only inline-error case, drawn as an error chip
//! in place while siblings render normally. Decode warnings never reach the
//! walker. Callers that decode (rather than construct) trees surface them
//! themselves.

use crate::{NodeCtx, node, response::DynResponse};
use gantz_ui::{Align, BindPath, Element, Key, Rgba};

/// The outcome of interpreting a UI tree for one frame.
///
/// Interpreting a tree never edits CA-affecting state (controls write VM
/// runtime state and queue evaluations only), so there is no `changed` flag.
#[derive(Debug, Default)]
pub struct UiTreeResponse {
    /// The union of every rendered widget's response, if anything rendered.
    pub inner: Option<egui::Response>,
    /// Payloads emitted for the application to handle after the GUI pass
    /// ([`EvalEntry`][crate::EvalEntry] push evaluations in v1).
    pub payloads: Vec<DynResponse>,
}

/// Renders a [`gantz_ui::Element`] tree, resolving bindings against VM state
/// through the given [`NodeCtx`].
///
/// This is the only seam through which the tree touches the VM: state access
/// goes via [`NodeCtx`], and everything else is reported on the returned
/// [`UiTreeResponse`].
pub struct UiTree<'a> {
    root_id: egui::Id,
    instance_prefix: &'a [node::Id],
}

/// Walker state threaded through one tree traversal.
struct Walk<'a> {
    instance_prefix: &'a [node::Id],
    /// Accumulated `(scope id ...)` prefixes enclosing the current element.
    scopes: Vec<node::Id>,
    resp: UiTreeResponse,
}

impl<'a> UiTree<'a> {
    /// Interpret a tree whose widget identities derive from `root_id`.
    pub fn new(root_id: egui::Id) -> Self {
        Self {
            root_id,
            instance_prefix: &[],
        }
    }

    /// The path prefix prepended to every binding, i.e. the path at which
    /// the tree's defining graph is instanced. Empty by default.
    pub fn instance_prefix(mut self, prefix: &'a [node::Id]) -> Self {
        self.instance_prefix = prefix;
        self
    }

    /// Render the tree, resolving bindings via `ctx`.
    pub fn show(self, tree: &Element, ctx: &mut NodeCtx, ui: &mut egui::Ui) -> UiTreeResponse {
        let mut walk = Walk {
            instance_prefix: self.instance_prefix,
            scopes: Vec::new(),
            resp: UiTreeResponse::default(),
        };
        walk.element(tree, self.root_id, ctx, ui);
        walk.resp
    }
}

impl Walk<'_> {
    /// Render one element with the given derived id.
    fn element(&mut self, e: &Element, id: egui::Id, ctx: &mut NodeCtx, ui: &mut egui::Ui) {
        match e {
            Element::Col(col) => {
                let cross = cross_align(col.align.unwrap_or(Align::Start));
                ui.with_layout(egui::Layout::top_down(cross), |ui| {
                    if let Some(gap) = col.gap {
                        ui.spacing_mut().item_spacing.y = gap;
                    }
                    self.children(&col.children, id, ctx, ui);
                });
            }
            Element::Row(row) => {
                // Mirror `Ui::horizontal`: seed the row with the interact
                // height so cross-alignment centres within one row, not
                // within all remaining vertical space.
                let cross = cross_align(row.align.unwrap_or(Align::Center));
                let initial = egui::vec2(
                    ui.available_size_before_wrap().x,
                    ui.spacing().interact_size.y,
                );
                ui.allocate_ui_with_layout(initial, egui::Layout::left_to_right(cross), |ui| {
                    if let Some(gap) = row.gap {
                        ui.spacing_mut().item_spacing.x = gap;
                    }
                    self.children(&row.children, id, ctx, ui);
                });
            }
            Element::Grid(grid) => {
                let cols = (grid.cols.max(1)) as usize;
                let mut g = egui::Grid::new(id).num_columns(cols);
                if let Some(gap) = grid.gap {
                    g = g.spacing([gap, gap]);
                }
                g.show(ui, |ui| {
                    for (ix, child) in grid.children.iter().enumerate() {
                        self.element(child, child_id(id, ix, child.key()), ctx, ui);
                        if (ix + 1) % cols == 0 {
                            ui.end_row();
                        }
                    }
                });
            }
            Element::Frame(frame) => {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.vertical(|ui| {
                        if let Some(title) = &frame.title {
                            ui.strong(title);
                        }
                        self.children(&frame.children, id, ctx, ui);
                    });
                });
            }
            Element::Sep(_) => {
                let r = ui.separator();
                self.merge(r);
            }
            Element::Space(space) => {
                let amount = space.amount.unwrap_or_else(|| {
                    let spacing = ui.spacing().item_spacing;
                    if ui.layout().main_dir().is_horizontal() {
                        spacing.x
                    } else {
                        spacing.y
                    }
                });
                ui.add_space(amount);
            }
            Element::Scope(scope) => {
                // No visuals: children render inline under the extended
                // binding prefix, with the scope id salting their identity.
                self.scopes.push(scope.id);
                let parent = id.with(("scope", scope.id));
                self.children(&scope.children, parent, ctx, ui);
                self.scopes.pop();
            }
            Element::Dialer(_) | Element::Toggle(_) | Element::Button(_) | Element::Matrix(_) => {
                let r = placeholder_chip(ui, e.tag());
                self.merge(r);
            }
            Element::Label(label) => {
                let mut text = egui::RichText::new(&label.text);
                if let Some(size) = label.size {
                    text = text.size(size);
                }
                if let Some(Rgba([r, g, b, a])) = label.color {
                    text = text.color(egui::Color32::from_rgba_unmultiplied(r, g, b, a));
                }
                // Non-selectable so surrounding node bodies stay draggable.
                let r = ui.add(egui::Label::new(text).selectable(false));
                self.merge(r);
            }
            Element::Value(value) => {
                let text = match &value.bind {
                    None => "∅".to_string(),
                    Some(bind) => match ctx.extract_value_at(&self.resolve(bind)) {
                        Ok(Some(val)) => format!("{val:?}"),
                        Ok(None) => "∅".to_string(),
                        Err(_) => "ERR".to_string(),
                    },
                };
                let mut label = egui::Label::new(text).selectable(false);
                label = if value.wrap {
                    label.wrap()
                } else {
                    label.extend()
                };
                let r = ui.add(label);
                self.merge(r);
            }
            Element::Plot(_) => {
                let r = placeholder_chip(ui, e.tag());
                self.merge(r);
            }
            Element::RefGui(ref_gui) => {
                let r = weak_chip(ui, &format!("ref-gui {}", ref_gui.id))
                    .on_hover_text("embedded reference GUIs resolve in a future version");
                self.merge(r);
            }
            Element::Error(err) => {
                let color = ui.visuals().error_fg_color;
                let r = egui::Frame::group(ui.style())
                    .stroke(egui::Stroke::new(1.0, color))
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(err.reason.to_string()).weak())
                                .selectable(false),
                        )
                    })
                    .inner
                    .on_hover_text(format!("tree path: {:?}", err.path.0));
                self.merge(r);
            }
        }
    }

    /// Render container children, deriving each child's id from `parent`.
    fn children(
        &mut self,
        children: &[Element],
        parent: egui::Id,
        ctx: &mut NodeCtx,
        ui: &mut egui::Ui,
    ) {
        for (ix, child) in children.iter().enumerate() {
            self.element(child, child_id(parent, ix, child.key()), ctx, ui);
        }
    }

    /// Resolve a binding to its full state-tree path.
    fn resolve(&self, bind: &BindPath) -> Vec<node::Id> {
        resolve_path(self.instance_prefix, &self.scopes, bind)
    }

    /// Union `r` into the response.
    fn merge(&mut self, r: egui::Response) {
        self.resp.inner = Some(match self.resp.inner.take() {
            Some(prev) => prev.union(r),
            None => r,
        });
    }
}

/// A child's identity: its structural position beneath `parent`, overridden
/// by a `key` attr. Key spaces are disjoint from position spaces, so keyed
/// and unkeyed siblings can never collide.
fn child_id(parent: egui::Id, position: usize, key: Option<&Key>) -> egui::Id {
    match key {
        Some(Key::Str(s)) => parent.with(("k", s.as_str())),
        Some(Key::Int(i)) => parent.with(("k", i)),
        None => parent.with(position),
    }
}

/// The full state-tree path of a binding: the tree's instance prefix, then
/// the accumulated scope prefixes, then the bind path itself.
fn resolve_path(prefix: &[node::Id], scopes: &[node::Id], bind: &BindPath) -> Vec<node::Id> {
    prefix
        .iter()
        .chain(scopes.iter())
        .chain(bind.0.iter())
        .copied()
        .collect()
}

/// Map a cross-axis alignment to egui's.
fn cross_align(align: Align) -> egui::Align {
    match align {
        Align::Start => egui::Align::Min,
        Align::Center => egui::Align::Center,
        Align::End => egui::Align::Max,
    }
}

/// An inline chip for elements that cannot render yet.
fn placeholder_chip(ui: &mut egui::Ui, text: &str) -> egui::Response {
    weak_chip(ui, text)
}

/// A small framed chip with weak text.
fn weak_chip(ui: &mut egui::Ui, text: &str) -> egui::Response {
    egui::Frame::group(ui.style())
        .show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(text).weak()).selectable(false))
        })
        .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_concatenates_prefix_scopes_and_bind() {
        let bind = BindPath(vec![7]);
        assert_eq!(resolve_path(&[], &[], &bind), vec![7]);
        assert_eq!(resolve_path(&[1], &[], &bind), vec![1, 7]);
        assert_eq!(resolve_path(&[], &[4], &bind), vec![4, 7]);
        assert_eq!(resolve_path(&[1, 2], &[3, 4], &bind), vec![1, 2, 3, 4, 7]);
        let deep = BindPath(vec![5, 6]);
        assert_eq!(resolve_path(&[1], &[2], &deep), vec![1, 2, 5, 6]);
    }

    #[test]
    fn keyed_children_keep_their_id_under_reorder() {
        let parent = egui::Id::new("parent");
        let key = Key::Str("a".to_string());
        assert_eq!(
            child_id(parent, 0, Some(&key)),
            child_id(parent, 5, Some(&key)),
        );
        assert_eq!(
            child_id(parent, 0, Some(&Key::Int(3))),
            child_id(parent, 9, Some(&Key::Int(3))),
        );
    }

    #[test]
    fn unkeyed_children_are_identified_by_position() {
        let parent = egui::Id::new("parent");
        assert_ne!(child_id(parent, 0, None), child_id(parent, 1, None));
        assert_eq!(child_id(parent, 2, None), child_id(parent, 2, None));
    }

    #[test]
    fn distinct_keys_are_distinct_ids() {
        let parent = egui::Id::new("parent");
        let a = Key::Str("a".to_string());
        let b = Key::Str("b".to_string());
        assert_ne!(child_id(parent, 0, Some(&a)), child_id(parent, 0, Some(&b)));
        assert_ne!(
            child_id(parent, 0, Some(&Key::Int(1))),
            child_id(parent, 0, Some(&Key::Int(2))),
        );
    }

    #[test]
    fn ids_derive_from_the_parent() {
        let (p1, p2) = (egui::Id::new("p1"), egui::Id::new("p2"));
        assert_ne!(child_id(p1, 0, None), child_id(p2, 0, None));
        let key = Key::Str("a".to_string());
        assert_ne!(child_id(p1, 0, Some(&key)), child_id(p2, 0, Some(&key)));
    }
}
