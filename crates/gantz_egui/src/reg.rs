//! Registry reference for node lookup and trait implementations.
//!
//! Provides [`RegistryRef`] - a unified view combining a content-addressed
//! registry with builtin nodes, implementing the [`Registry`] trait required
//! by the gantz_egui widgets.

use crate::Registry;
use gantz_ca as ca;
use gantz_core::data::ReifiedGraphs;
use gantz_core::node;
use gantz_core::{Builtins, Node};
use petgraph::visit::{IntoNodeReferences, NodeRef};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

/// Registry reference providing unified node access.
///
/// Combines access to a content-addressed registry (for user-defined graphs,
/// stored as data), the reified-graph cache serving those graphs as typed
/// nodes, and the builtin palette ([`Builtins`] data plus one reified
/// instance per builtin), implementing the [`Registry`] trait required by
/// the gantz_egui widgets.
pub struct RegistryRef<'a, N: 'static + Send + Sync> {
    ca_registry: &'a ca::Registry,
    reified: &'a ReifiedGraphs<N>,
    builtins: &'a Builtins,
    instances: &'a HashMap<ca::ContentAddr, N>,
}

/// Named heads (branches), ordered by name.
pub type Names = BTreeMap<ca::Name, ca::CommitAddr>;

impl<'a, N: 'static + Send + Sync> RegistryRef<'a, N> {
    /// Construct from a CA registry, its reified-graph cache, the builtin
    /// palette data and its reified instances (keyed by erased content
    /// address, one per builtin).
    pub fn new(
        ca_registry: &'a ca::Registry,
        reified: &'a ReifiedGraphs<N>,
        builtins: &'a Builtins,
        instances: &'a HashMap<ca::ContentAddr, N>,
    ) -> Self {
        Self {
            ca_registry,
            reified,
            builtins,
            instances,
        }
    }

    /// Access the underlying CA registry.
    pub fn ca_registry(&self) -> &ca::Registry {
        self.ca_registry
    }

    /// Access the reified-graph cache.
    pub fn reified(&self) -> &ReifiedGraphs<N> {
        self.reified
    }

    /// Access the builtin palette data.
    pub fn builtins(&self) -> &Builtins {
        self.builtins
    }

    /// Access the reified builtin instances.
    pub fn instances(&self) -> &HashMap<ca::ContentAddr, N> {
        self.instances
    }
}

impl<N: 'static + Node + Send + Sync> RegistryRef<'_, N> {
    /// Look up a node by content address.
    ///
    /// Checks reified registry graphs first (a graph in the registry IS a
    /// node), then falls back to builtins.
    ///
    /// Also available via [`Registry::node`]; this inherent form asks only
    /// `N: Node` of the node type, for callers (e.g. headless graph ops) whose
    /// node bounds don't meet the full [`Registry`] impl's UI requirements.
    pub fn node(&self, ca: &ca::ContentAddr) -> Option<&dyn Node> {
        let graph_ca = ca::GraphAddr::from(*ca);
        if let Some(graph) = self.reified.get(&graph_ca) {
            return Some(graph as &dyn Node);
        }
        self.instances.get(ca).map(|n| n as &dyn Node)
    }

    /// Create a node of the given type name.
    ///
    /// Checks registry names first (creating a [`crate::node::NamedRef`]
    /// pinning the name's head graph), then falls back to builtins,
    /// reifying a fresh instance from the builtin's stored data.
    pub fn create_node(&self, node_type: &str) -> Option<N>
    where
        N: From<crate::node::NamedRef> + serde::de::DeserializeOwned,
    {
        let name: ca::Name = node_type.parse().expect("infallible");
        head_graph_addr(self.ca_registry, &name)
            .map(|graph_addr| {
                let ref_ = gantz_core::node::Ref::new(graph_addr.into());
                let named = crate::node::NamedRef::new(name.clone(), ref_);
                N::from(named)
            })
            .or_else(|| {
                let node_data = self.builtins.node_data(node_type)?;
                match gantz_core::data::reify_node(node_data) {
                    Ok(node) => Some(node),
                    Err(e) => {
                        log::error!("builtin `{node_type}` failed to reify: {e}");
                        None
                    }
                }
            })
    }
}

impl<N> Registry for RegistryRef<'_, N>
where
    N: 'static + Node + crate::NodeUi + Send + Sync,
{
    fn ca(&self) -> &ca::Registry {
        self.ca_registry
    }

    fn node(&self, ca: &ca::ContentAddr) -> Option<&dyn Node> {
        RegistryRef::node(self, ca)
    }

    fn name_ca(&self, name: &str) -> Option<ca::ContentAddr> {
        let parsed: ca::Name = name.parse().expect("infallible");
        head_graph_addr(self.ca_registry, &parsed)
            .map(Into::into)
            .or_else(|| self.builtins.content_addr(name))
    }

    fn node_exists(&self, ca: &ca::ContentAddr) -> bool {
        self.node(ca).is_some()
    }

    fn fn_node_names(&self) -> Vec<String> {
        let builtin_names = self.builtins.names().map(str::to_string);
        let registry_names = self.ca_registry.heads().map(|(name, _)| name.to_string());
        let all_names = builtin_names.chain(registry_names);

        let get_node = |ca: &ca::ContentAddr| self.node(ca);
        let mut names: Vec<_> = all_names
            .filter(|name| {
                let meta_ctx = node::MetaCtx::new(&get_node);
                self.name_ca(name)
                    .and_then(|ca| self.node(&ca))
                    .map(|n| {
                        !n.stateful(meta_ctx)
                            && n.branches(meta_ctx).is_empty()
                            && n.n_outputs(meta_ctx) == 1
                    })
                    .unwrap_or(false)
            })
            .collect();

        names.sort();
        names
    }

    fn node_types(&self) -> Vec<&str> {
        // The reserved nested-graph entry replaces the old `graph` builtin.
        let mut types = vec![crate::widget::gantz::NESTED_GRAPH_TYPE];
        for name in self.builtins.names() {
            types.push(name);
        }
        // Nested graphs are hidden from the root graph-select list, so don't
        // offer them as creatable node types either. A root name is its
        // single segment.
        types.extend(
            self.ca_registry
                .heads()
                .filter(|(name, _)| !name.is_nested())
                .map(|(name, _)| name.segments()[0].as_str()),
        );
        types.sort();
        types.dedup();
        types
    }

    fn would_ref_cycle(&self, target: &str, editing: &str) -> bool {
        let target: ca::Name = target.parse().expect("infallible");
        let editing: ca::Name = editing.parse().expect("infallible");
        crate::cycle::would_cycle(self.ca_registry, &target, &editing)
    }

    fn demo_graph(&self, name: &str) -> Option<String> {
        // Demo associations live in the registry's demo section, keyed by name.
        let parsed: ca::Name = name.parse().expect("infallible");
        crate::section::demo(self.ca_registry, &parsed)
    }

    fn socket_doc(
        &self,
        ca: &ca::ContentAddr,
        kind: crate::SocketKind,
        ix: usize,
    ) -> Option<crate::SocketDoc> {
        // Resolve the referenced graph and read the ix-th inlet/outlet marker's
        // own doc (docs live on the `Inlet`/`Outlet` nodes).
        let graph_ca = ca::GraphAddr::from(*ca);
        let graph = self.reified.get(&graph_ca)?;
        let get_node = |c: &ca::ContentAddr| self.node(c);
        let meta_ctx = node::MetaCtx::new(&get_node);
        let node_ref = graph
            .node_references()
            .filter(|n| match kind {
                crate::SocketKind::Input => n.weight().inlet(meta_ctx),
                crate::SocketKind::Output => n.weight().outlet(meta_ctx),
            })
            .nth(ix)?;
        let marker = node_ref.weight();
        // An inlet exposes its doc on its output socket; an outlet on its input.
        let marker_kind = match kind {
            crate::SocketKind::Input => crate::SocketKind::Output,
            crate::SocketKind::Output => crate::SocketKind::Input,
        };
        marker.socket_doc(self, marker_kind, 0)
    }

    fn command_info(&self, name: &str) -> crate::CommandInfo {
        use crate::SocketKind;
        let mut info = crate::CommandInfo {
            name: name.to_string(),
            description: self.node_description(name),
            ..Default::default()
        };

        // The reserved nested-graph entry mints a fresh, empty child graph.
        if name == crate::widget::gantz::NESTED_GRAPH_TYPE {
            return info;
        }

        let get_node = |c: &ca::ContentAddr| self.node(c);
        let meta_ctx = node::MetaCtx::new(&get_node);
        // Collect `n` socket docs, defaulting a missing doc to a bare "any".
        let collect =
            |n: usize,
             kind: SocketKind,
             f: &dyn Fn(SocketKind, usize) -> Option<crate::SocketDoc>| {
                (0..n)
                    .map(|ix| f(kind, ix).unwrap_or_else(|| crate::SocketDoc::ty("any")))
                    .collect::<Vec<_>>()
            };

        let parsed: ca::Name = name.parse().expect("infallible");
        if let Some(graph_addr) = head_graph_addr(self.ca_registry, &parsed) {
            // A named graph: socket docs resolved from the referenced graph's
            // inlet/outlet markers.
            if let Some(graph) = self.reified.get(&graph_addr) {
                let ca: ca::ContentAddr = graph_addr.into();
                let socket = |kind: SocketKind, ix: usize| self.socket_doc(&ca, kind, ix);
                info.inputs = collect(graph.n_inputs(meta_ctx), SocketKind::Input, &socket);
                info.outputs = collect(graph.n_outputs(meta_ctx), SocketKind::Output, &socket);
            }
        } else if let Some(builtin) = self
            .builtins
            .content_addr(name)
            .and_then(|ca| self.instances.get(&ca))
        {
            // A builtin: introspect its stored instance.
            let socket = |kind: SocketKind, ix: usize| builtin.socket_doc(self, kind, ix);
            info.inputs = collect(builtin.n_inputs(meta_ctx), SocketKind::Input, &socket);
            info.outputs = collect(builtin.n_outputs(meta_ctx), SocketKind::Output, &socket);
        }

        info
    }

    fn merge_preview(
        &self,
        ours: &ca::Head,
        source: &str,
        resolutions: ca::Resolutions,
    ) -> Option<crate::merge::MergePreview> {
        crate::merge::merge_preview(self.ca_registry, ours, source, resolutions)
    }

    fn node_description(&self, name: &str) -> Option<Cow<'static, str>> {
        if name == crate::widget::gantz::NESTED_GRAPH_TYPE {
            return Some(Cow::Borrowed(
                "Create a new nested graph. Its inlets and outlets become this node's sockets.",
            ));
        }
        let parsed: ca::Name = name.parse().expect("infallible");
        if self.ca_registry.head(&parsed).is_some() {
            return crate::section::description(self.ca_registry, &parsed).map(Cow::Owned);
        }
        self.builtins
            .content_addr(name)
            .and_then(|ca| self.instances.get(&ca))
            .and_then(|n| n.description())
            .map(Cow::Borrowed)
    }
}

/// The graph address at the tip of the named line of history: the address a
/// [`Ref`](gantz_core::node::Ref) to the name should pin.
pub fn head_graph_addr(reg: &ca::Registry, name: &ca::Name) -> Option<ca::GraphAddr> {
    let head_ca = reg.head(name)?;
    reg.commits().get(&head_ca).map(|commit| commit.graph)
}

/// All name -> head commit pairs, in name order.
pub fn names(reg: &ca::Registry) -> Vec<(ca::Name, ca::CommitAddr)> {
    reg.heads().map(|(name, ca)| (name.clone(), ca)).collect()
}
