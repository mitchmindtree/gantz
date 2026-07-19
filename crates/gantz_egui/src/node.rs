//! Provides custom nodes that are commonly useful to egui applications of
//! gantz.
//!
//! Provides new node items, while re-exporting some of the `gantz_core::node`
//! items for convenience.

pub use comment::Comment;
pub use dyn_node::{DynNode, NodeCodec, NormalizeNodeError, UiNodeInstance};
pub use fn_named_ref::FnNamedRef;
#[doc(inline)]
pub use gantz_core::node::{Id, state};
pub use inspect::Inspect;
pub use named_ref::{NamedRef, missing_color, outdated_color};
pub use plot::{Plot, PlotMode, PlotStyle};
pub use ref_ext::RefExtUi;

pub mod comment;
pub mod dyn_node;
pub mod fn_named_ref;
pub mod inspect;
pub mod named_ref;
pub mod plot;
pub mod ref_ext;
mod size_sync;

/// Builtin specs for the egui node set.
pub fn builtins<N>() -> Vec<gantz_core::Builtin<N>>
where
    N: gantz_core::FromNode<Comment>
        + gantz_core::FromNode<FnNamedRef>
        + gantz_core::FromNode<Inspect>
        + gantz_core::FromNode<Plot>,
{
    use gantz_core::Builtin;
    // The `fn` builtin defaults to referring to the `identity` builtin.
    let identity_ca = gantz_ca::content_addr(&gantz_core::node::Identity);
    vec![
        Builtin::new("comment", || N::from_node(Comment::default())),
        Builtin::new("fn", move || {
            let name = gantz_core::node::IDENTITY_NAME.parse().expect("infallible");
            let named_ref = NamedRef::new(name, gantz_core::node::Ref::new(identity_ca));
            N::from_node(gantz_core::node::Fn::new(named_ref))
        }),
        Builtin::new("inspect", || N::from_node(Inspect::default())),
        Builtin::new("plot", || N::from_node(Plot::default())),
    ]
}
