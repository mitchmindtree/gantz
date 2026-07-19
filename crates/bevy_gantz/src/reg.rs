//! Graph registry resources and node lookup.
//!
//! Provides:
//! - [`Registry`] — Bevy resource wrapping the data-level `gantz_ca::Registry`
//! - [`GraphCache<N>`] — Bevy resource wrapping the reified-graph cache
//! - [`lookup_node`] — Simple node lookup function

use crate::builtin::Builtins;
use crate::head::{HeadRef, OpenHead};
use bevy_ecs::prelude::*;
use bevy_log as log;
use gantz_ca as ca;
use gantz_ca::DataGraph;
use gantz_core::Node;
use gantz_core::data::ReifiedGraphs;
use serde::de::DeserializeOwned;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Registry resource
// ---------------------------------------------------------------------------

/// A `Resource` wrapper around the data-level [`gantz_ca::Registry`].
///
/// The registry stores graphs as concrete data ([`DataGraph`]); typed graphs
/// are served from the [`GraphCache`].
#[derive(Default, Resource)]
pub struct Registry(pub ca::Registry<DataGraph>);

/// A `Resource` wrapper around the append-only reified-graph cache serving
/// the registry's graphs as typed `Graph<N>`s.
///
/// Keep it in step with the registry via [`refresh_cache`] wherever the
/// registry is mutated before typed reads happen.
#[derive(Resource)]
pub struct GraphCache<N>(pub ReifiedGraphs<N>);

impl std::ops::Deref for Registry {
    type Target = ca::Registry<DataGraph>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Registry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<N> Default for GraphCache<N> {
    fn default() -> Self {
        Self(ReifiedGraphs::new())
    }
}

impl<N> std::ops::Deref for GraphCache<N> {
    type Target = ReifiedGraphs<N>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<N> std::ops::DerefMut for GraphCache<N> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Create a timestamp for a commit (current time since UNIX epoch).
pub fn timestamp() -> Duration {
    let now = web_time::SystemTime::now();
    now.duration_since(web_time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
}

// ---------------------------------------------------------------------------
// Node lookup
// ---------------------------------------------------------------------------

/// Look up a node by content address.
///
/// Checks reified registry graphs first (a graph in the registry IS a node),
/// then falls back to builtins.
pub fn lookup_node<'a, N: 'static + Node + Send + Sync>(
    cache: &'a ReifiedGraphs<N>,
    builtins: &'a dyn Builtins<Node = N>,
    ca: &ca::ContentAddr,
) -> Option<&'a dyn Node> {
    let graph_ca = ca::GraphAddr::from(*ca);
    if let Some(graph) = cache.get(&graph_ca) {
        return Some(graph as &dyn Node);
    }
    builtins.instance(ca).map(|n| n as &dyn Node)
}

/// Bring the reified-graph cache up to date with the registry, best effort.
///
/// Graphs that fail to reify (e.g. an unknown tag from a domain not compiled
/// in) are logged and remain cache misses that typed lookups degrade over.
pub fn refresh_cache<N: DeserializeOwned>(reg: &Registry, cache: &mut GraphCache<N>) {
    for e in cache.0.ensure_all(reg) {
        log::error!("failed to reify registry graph: {e}");
    }
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

/// Prune unreachable content and metadata from the registry, dropping the
/// pruned graphs' cache entries with them.
pub fn prune_unused<N>(
    mut registry: ResMut<Registry>,
    mut cache: ResMut<GraphCache<N>>,
    heads: Query<&HeadRef, With<OpenHead>>,
) where
    N: 'static + Send + Sync,
{
    let extra = heads
        .iter()
        .filter_map(|h| registry.head_commit_ca(&**h))
        .collect::<Vec<_>>();
    let live = ca::closure(&registry.0, extra, ca::data_graph_out);
    ca::prune(&mut registry.0, &live);
    cache.retain_live(&live);
}
