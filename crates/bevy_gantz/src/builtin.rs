//! The builtin palette as a Bevy resource.

use bevy_ecs::prelude::*;
use gantz_ca::ContentAddr;
pub use gantz_core::Builtins;
use gantz_core::data::ReifyNodeError;
use serde::de::DeserializeOwned;
use std::collections::HashMap;

/// Resource carrying the app's builtin palette: the composed [`Builtins`]
/// data plus one reified node instance per builtin, keyed by its erased
/// content address.
///
/// The instances live here (rather than in the egui layer) because
/// compilation's node lookup ([`crate::lookup_node`]) needs the
/// addr -> `&dyn Node` fallback; the same instances also serve the egui
/// layer's UI introspection through the node type's `NodeUi` impl - one map,
/// no dependency on the UI crates.
#[derive(Resource)]
pub struct BuiltinNodes<N: 'static + Send + Sync> {
    /// The composed builtin palette as data.
    pub builtins: Builtins,
    /// One reified instance per builtin, keyed by erased content address.
    pub instances: HashMap<ContentAddr, N>,
}

impl<N: 'static + Send + Sync> BuiltinNodes<N> {
    /// Reify one `N` instance per builtin through the node set's serde.
    ///
    /// Failures are returned for logging; a builtin that fails to reify
    /// (e.g. a tag from a domain not compiled in) degrades to a lookup miss.
    pub fn reify(builtins: Builtins) -> (Self, Vec<ReifyNodeError>)
    where
        N: DeserializeOwned,
    {
        let mut instances = HashMap::new();
        let mut errs = vec![];
        for name in builtins.names() {
            let node_data = builtins.node_data(name).expect("named builtin");
            match gantz_core::data::reify_node(node_data) {
                Ok(node) => {
                    instances.insert(node_data.content_addr(), node);
                }
                Err(e) => errs.push(e),
            }
        }
        (
            Self {
                builtins,
                instances,
            },
            errs,
        )
    }
}

impl<N: 'static + Send + Sync> Default for BuiltinNodes<N> {
    fn default() -> Self {
        Self {
            builtins: Builtins::default(),
            instances: HashMap::new(),
        }
    }
}
