//! The erased UI node and the value-level codec between typed nodes and the
//! registry's [`NodeData`] representation.
//!
//! [`NodeUi`]'s [`gantz_core::Node`] supertrait makes [`DynNode`] a
//! self-sufficient working-graph weight: one erased value serves rendering,
//! compilation and evaluation. The [`NodeCodec`] carries the node set as
//! *values* rather than as a `Box<dyn Trait>` serde impl: an application
//! composes one with [`ui_node_codec!`](crate::ui_node_codec) from the same
//! type manifest as its `gantz_format::impl_node_set_serde!` invocation, and
//! both paths produce byte-identical [`NodeData`] (the same content
//! addresses).

use crate::NodeUi;
use gantz_ca::{DataGraph, NodeData};
use gantz_core::data::{EraseNodeError, ReifyError, ReifyNodeError};
use gantz_core::node::graph::Graph;
use petgraph::visit::EdgeRef;
use std::any::Any;

/// A UI-capable node erased to a trait object.
///
/// Via [`NodeUi`]'s supertrait, `DynNode` implements [`gantz_core::Node`]
/// (and `NodeUi` itself), so it slots directly into [`Graph`] weights,
/// compilation and the widgets.
pub type DynNode = Box<dyn NodeUi>;

/// A reified node paired with the monomorphic eraser for its concrete type.
///
/// Produced by [`NodeCodec::reify_ui`]: the codec arm that decoded the node
/// knows its concrete type, so it captures an eraser that downcasts and runs
/// [`erase_node_typed`](gantz_core::data::erase_node_typed) - no trait-object
/// serde involved.
pub struct UiNodeInstance {
    /// The reified node.
    pub node: DynNode,
    erase: fn(&dyn Any) -> Result<NodeData, EraseNodeError>,
}

/// A value-level codec between a node set's typed nodes and their stored
/// [`NodeData`] form.
///
/// Two plain function pointers, so a codec is `Copy` and can be composed as
/// a `const`. Build one with [`ui_node_codec!`](crate::ui_node_codec).
#[derive(Clone, Copy)]
pub struct NodeCodec {
    reify: fn(&NodeData) -> Result<UiNodeInstance, ReifyNodeError>,
    sugars: fn() -> gantz_format::Sugars<'static>,
}

/// Failure to normalize a node's data form through its type: the reify or
/// the re-erasure failed.
#[derive(Clone, Debug, thiserror::Error)]
pub enum NormalizeNodeError {
    /// The stored form failed to decode (unknown tag or invalid fields).
    #[error(transparent)]
    Reify(#[from] ReifyNodeError),
    /// The reified node failed to erase back to data.
    #[error(transparent)]
    Erase(#[from] EraseNodeError),
}

impl UiNodeInstance {
    /// Pair a reified node with its concrete type's eraser.
    ///
    /// `erase` receives the node upcast to `&dyn Any` and must downcast to
    /// the node's own concrete type - it is the codec arm's responsibility
    /// (see [`ui_node_codec!`](crate::ui_node_codec)) that the two agree.
    pub fn new(node: DynNode, erase: fn(&dyn Any) -> Result<NodeData, EraseNodeError>) -> Self {
        Self { node, erase }
    }

    /// Erase the node back to its canonical data form (see
    /// [`erase_node_typed`](gantz_core::data::erase_node_typed)).
    pub fn erase(&self) -> Result<NodeData, EraseNodeError> {
        let node: &dyn gantz_core::Node = &*self.node;
        let any: &dyn Any = node;
        (self.erase)(any)
    }
}

impl NodeCodec {
    /// Compose a codec from its reify dispatch and its node set's sugar
    /// source. See [`ui_node_codec!`](crate::ui_node_codec) for the standard
    /// construction.
    pub const fn new(
        reify: fn(&NodeData) -> Result<UiNodeInstance, ReifyNodeError>,
        sugars: fn() -> gantz_format::Sugars<'static>,
    ) -> Self {
        Self { reify, sugars }
    }

    /// Reify one stored node to a typed [`UiNodeInstance`].
    pub fn reify_ui(&self, node_data: &NodeData) -> Result<UiNodeInstance, ReifyNodeError> {
        (self.reify)(node_data)
    }

    /// Reify a stored graph: node weights through [`reify_ui`][Self::reify_ui],
    /// indices and edges preserved verbatim (mirrors [`gantz_core::data::reify`]).
    pub fn reify_graph(&self, dg: &DataGraph) -> Result<Graph<DynNode>, ReifyError> {
        let mut out = Graph::with_capacity(dg.node_count(), dg.edge_count());
        for (node_ix, node_data) in dg.node_weights().enumerate() {
            let inst = self
                .reify_ui(node_data)
                .map_err(|source| ReifyError { node_ix, source })?;
            out.add_node(inst.node);
        }
        for e in dg.edge_references() {
            out.add_edge(e.source(), e.target(), *e.weight());
        }
        Ok(out)
    }

    /// Round-trip a node's data form through its type: reify, then erase.
    ///
    /// Validates the fields against the node's own serde and recomputes the
    /// canonical form and the refs/blobs columns from the node's reporting.
    pub fn normalize(&self, node_data: &NodeData) -> Result<NodeData, NormalizeNodeError> {
        Ok(self.reify_ui(node_data)?.erase()?)
    }

    /// The node set's composed `.gantz` keyword sugar.
    pub fn sugars(&self) -> gantz_format::Sugars<'static> {
        (self.sugars)()
    }
}

/// Compose a [`NodeCodec`](crate::node::NodeCodec) over a node set.
///
/// Takes the node set's `gantz_format::NodeSugar` carrier type and the list
/// of node types - the SAME manifest as the application's
/// `gantz_format::impl_node_set_serde!` invocation, so the value-level codec
/// and the box serde agree on tags, wire shapes and content addresses (gate
/// the two against each other with a round-trip test). Each listed type must
/// implement `gantz_nodetag::NodeTag`, `serde::Serialize`,
/// `serde::de::DeserializeOwned` and [`NodeUi`](crate::NodeUi); the calling
/// crate must depend on `serde`.
///
/// Reifying data whose tag is not listed fails with a
/// `gantz_core::data::ReifyNodeError` naming the tag.
///
/// ```ignore
/// pub fn codec() -> gantz_egui::node::NodeCodec {
///     gantz_egui::ui_node_codec! {
///         Box<dyn Node> {
///             gantz_core::node::Expr,
///             gantz_egui::node::Comment,
///             // ...
///         }
///     }
/// }
/// ```
#[macro_export]
macro_rules! ui_node_codec {
    ($carrier:ty { $($ty:ty),+ $(,)? }) => {{
        fn reify(
            node_data: &$crate::gantz_ca::NodeData,
        ) -> ::std::result::Result<
            $crate::node::UiNodeInstance,
            $crate::gantz_core::data::ReifyNodeError,
        > {
            $(
                if node_data.tag == <$ty as $crate::gantz_nodetag::NodeTag>::TAG {
                    return $crate::gantz_core::data::reify_node_concrete::<$ty>(node_data)
                        .map(|node| $crate::node::UiNodeInstance::new(
                            ::std::boxed::Box::new(node),
                            |any: &dyn ::std::any::Any| {
                                let node = any
                                    .downcast_ref::<$ty>()
                                    .expect("tag-matched type");
                                $crate::gantz_core::data::erase_node_typed(node)
                            },
                        ));
                }
            )+
            ::std::result::Result::Err($crate::gantz_core::data::ReifyNodeError {
                tag: node_data.tag.clone(),
                source: <$crate::gantz_ca::DatumError as ::serde::de::Error>::custom(
                    "unknown node type tag: not listed in `ui_node_codec!`",
                ),
            })
        }
        $crate::node::NodeCodec::new(
            reify,
            <$carrier as $crate::gantz_format::NodeSugar>::sugar,
        )
    }};
}
