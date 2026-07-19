//! Keeping `NamedRef` references current across the registry.
//!
//! When a named graph is edited it commits to a new address; every graph that
//! references it by name must then follow. [`resync`] brings all sync-enabled
//! [`NamedRef`]s up to their name's current commit,
//! recommitting any graph whose references changed. This is the headless
//! counterpart of the inspector's render-time auto-sync, and the mechanism by
//! which editing a nested graph propagates up to its parents.

use crate::node::NamedRef;
use gantz_ca::{CommitAddr, DataGraph, GraphAddr, Name, Registry};
use gantz_core::node::graph::Graph;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::time::Duration;

/// Access the [`NamedRef`] within a frontend's node type, if any.
///
/// Implemented by frontends (typically a downcast) so the otherwise node-type-
/// agnostic [`resync`] / rename machinery can find and update references. This
/// is the references-only analogue of the removed `ToGraphMut`.
pub trait AsNamedRefMut {
    /// A mutable reference to the inner [`NamedRef`], if this node is one.
    fn as_named_ref_mut(&mut self) -> Option<&mut NamedRef>;
}

/// Access the [`NamedRef`] within a frontend's node type, if any.
///
/// The read-only companion to [`AsNamedRefMut`], used by the reference-cycle
/// check (`crate::cycle`) to walk a graph's named references without mutating.
pub trait AsNamedRef {
    /// A shared reference to the inner [`NamedRef`], if this node is one.
    fn as_named_ref(&self) -> Option<&NamedRef>;
}

/// A named graph whose commit moved during [`resync`] or a rename cascade.
#[derive(Clone, Debug)]
pub struct Moved {
    /// The name whose commit moved.
    pub name: Name,
    /// The commit the name pointed at before.
    pub old_commit: CommitAddr,
    /// The commit the name points at now.
    pub new_commit: CommitAddr,
}

/// Reify the stored graph at `source_commit` through the node set, logging
/// (and skipping, via `None`) graphs that fail to decode.
fn reify_commit_graph<N>(
    registry: &Registry<DataGraph>,
    source_commit: &CommitAddr,
) -> Option<Graph<N>>
where
    N: DeserializeOwned,
{
    let data_graph = registry.commit_graph_ref(source_commit)?;
    match gantz_core::data::reify(data_graph) {
        Ok(g) => Some(g),
        Err(e) => {
            log::error!("failed to reify graph at commit {source_commit}: {e}");
            None
        }
    }
}

/// Apply `mutate` to every inner [`NamedRef`] of `graph`, returning whether
/// any reference changed.
fn rewrite_refs<N>(graph: &mut Graph<N>, mut mutate: impl FnMut(&mut NamedRef) -> bool) -> bool
where
    N: AsNamedRefMut,
{
    let mut changed = false;
    for weight in graph.node_weights_mut() {
        if let Some(named_ref) = weight.as_named_ref_mut() {
            changed |= mutate(named_ref);
        }
    }
    changed
}

/// Erase a rewritten graph and commit it under `name`, returning the new
/// commit. Logs (and skips, via `None`) graphs that fail to erase.
fn commit_erased<N>(
    registry: &mut Registry<DataGraph>,
    timestamp: Duration,
    name: &Name,
    graph: &Graph<N>,
) -> Option<CommitAddr>
where
    N: Serialize + gantz_core::Node,
{
    let (data_graph, graph_ca) = match gantz_core::data::erase_with_addr(graph) {
        Ok(pair) => pair,
        Err(e) => {
            log::error!("failed to erase rewritten graph for `{name}`: {e}");
            return None;
        }
    };
    Some(registry.commit_graph_to_name(timestamp, graph_ca, || data_graph, name))
}

/// Rewrite the references in the graph at `source_commit` via `mutate`, and -
/// when something changed - commit the result under `name`. Returns the
/// resulting [`Moved`], or `None` when nothing changed.
fn commit_rewritten<N>(
    registry: &mut Registry<DataGraph>,
    timestamp: Duration,
    name: &Name,
    source_commit: CommitAddr,
    mutate: impl FnMut(&mut NamedRef) -> bool,
) -> Option<Moved>
where
    N: Serialize + DeserializeOwned + gantz_core::Node + AsNamedRefMut,
{
    let mut g: Graph<N> = reify_commit_graph(registry, &source_commit)?;
    if !rewrite_refs(&mut g, mutate) {
        return None;
    }
    let new_commit = commit_erased(registry, timestamp, name, &g)?;
    Some(Moved {
        name: name.clone(),
        old_commit: source_commit,
        new_commit,
    })
}

/// Repoint a [`NamedRef`] whose name was renamed, per a `old -> (new, graph)`
/// map. Returns whether it changed.
fn remap_ref(named_ref: &mut NamedRef, remap: &HashMap<Name, (Name, GraphAddr)>) -> bool {
    match remap.get(named_ref.name()) {
        Some((new_name, new_graph)) => {
            named_ref.rename(new_name.clone(), (*new_graph).into());
            true
        }
        None => false,
    }
}

/// The renamed counterpart of `descendant` when `old` is renamed to `new`:
/// `new`'s segments followed by `descendant`'s segments past `old`'s.
fn renamed(descendant: &Name, old: &Name, new: &Name) -> Name {
    let segments: Vec<String> = new
        .segments()
        .iter()
        .chain(&descendant.segments()[old.segments().len()..])
        .cloned()
        .collect();
    Name::from(segments)
}

/// Give a freshly-forked graph independent nested children.
///
/// Forking `old` to `new` copies `old`'s graph (done by the caller), but that
/// copy still references `old`'s nested children (`old:*`). This copies the
/// whole `old:*` subtree to `new:*` and rewrites the references so editing the
/// fork's nested graphs no longer affects the original. Returns the named
/// graphs whose commits were created or moved (the `new` root plus each
/// `new:*` child), so callers can refresh the open fork and migrate views.
///
/// Children are copied deepest-first so a parent's references resolve to its
/// already-copied children.
pub fn fork_nested<N>(
    registry: &mut Registry<DataGraph>,
    timestamp: Duration,
    old: &Name,
    new: &Name,
) -> Vec<Moved>
where
    N: Serialize + DeserializeOwned + gantz_core::Node + AsNamedRefMut,
{
    let mut descendants: Vec<Name> = registry
        .heads()
        .filter(|(n, _)| n.starts_with(old) && *n != old)
        .map(|(n, _)| n.clone())
        .collect();
    descendants.sort_by(|a, b| b.depth().cmp(&a.depth()).then_with(|| a.cmp(b)));

    // old descendant name -> (new name, new graph).
    let mut remap: HashMap<Name, (Name, GraphAddr)> = HashMap::new();
    let mut moves = Vec::new();

    // Each descendant is copied (under a fresh `new:*` name) with its references
    // to already-copied descendants repointed.
    for d in &descendants {
        let d_new = renamed(d, old, new);
        let Some(commit) = registry.head(d) else {
            continue;
        };
        let Some(mut g) = reify_commit_graph::<N>(registry, &commit) else {
            continue;
        };
        rewrite_refs(&mut g, |nr| remap_ref(nr, &remap));
        let Some(new_commit) = commit_erased(registry, timestamp, &d_new, &g) else {
            continue;
        };
        let graph_ca = registry
            .commits()
            .get(&new_commit)
            .expect("freshly committed")
            .graph;
        remap.insert(d.clone(), (d_new.clone(), graph_ca));
        moves.push(Moved {
            name: d_new,
            old_commit: commit,
            new_commit,
        });
    }

    // Repoint the already-created `new` root's references to its own children.
    if let Some(root_commit) = registry.head(new) {
        moves.extend(commit_rewritten::<N>(
            registry,
            timestamp,
            new,
            root_commit,
            |nr| remap_ref(nr, &remap),
        ));
    }

    moves
}

/// Bring every sync-enabled [`NamedRef`] in the registry up to its name's
/// current commit, recommitting any named graph whose references changed.
///
/// Returns the named graphs whose commits moved, so callers can refresh open
/// heads and migrate their views.
///
/// Graphs are processed deepest-name-first so a parent observes its children's
/// new commits within a single pass; a bounded fixpoint loop covers any
/// non-nesting reference shape. The loop cannot run forever even for a
/// (degenerate) mutually-referencing registry - it simply stops once no graph
/// changes.
pub fn resync<N>(registry: &mut Registry<DataGraph>, timestamp: Duration) -> Vec<Moved>
where
    N: Serialize + DeserializeOwned + gantz_core::Node + AsNamedRefMut,
{
    // Deepest names first: a child is updated before the parent that refs it.
    let mut order: Vec<Name> = registry.heads().map(|(n, _)| n.clone()).collect();
    order.sort_by(|a, b| b.depth().cmp(&a.depth()).then_with(|| a.cmp(b)));

    // A name -> current head graph snapshot, kept in step with commits we
    // make so a referrer resolves its children to their freshly-committed
    // content.
    let mut current: HashMap<Name, (CommitAddr, GraphAddr)> = registry
        .heads()
        .filter_map(|(n, ca)| {
            let graph = registry.commits().get(&ca)?.graph;
            Some((n.clone(), (ca, graph)))
        })
        .collect();

    let mut moves = Vec::new();
    let max_passes = order.len() + 1;
    for _ in 0..max_passes {
        let mut changed_any = false;
        for name in &order {
            let Some(&(commit_ca, _)) = current.get(name) else {
                continue;
            };
            let resolve = |m: &Name| {
                current
                    .get(m)
                    .map(|&(_, graph)| gantz_ca::ContentAddr::from(graph))
            };
            if let Some(moved) = commit_rewritten::<N>(registry, timestamp, name, commit_ca, |nr| {
                nr.resync(&resolve)
            }) {
                let graph = registry
                    .commits()
                    .get(&moved.new_commit)
                    .expect("freshly committed")
                    .graph;
                current.insert(name.clone(), (moved.new_commit, graph));
                moves.push(moved);
                changed_any = true;
            }
        }
        if !changed_any {
            break;
        }
    }
    moves
}

/// Promote a nested graph that was renamed to a (root) name: repoint its
/// parent's references from the old nested name to `new_name`, then drop the
/// now-orphaned nested name and its descendants.
///
/// `old_nested` is the renamed graph's former `parent:child` name; `new_name`
/// is its new root name (a fresh copy of its graph already committed under it).
/// The parent may hold *several* references to the nested graph - each an
/// independent instance with its own state - and they are all repointed.
/// Returns the parent's move (if it changed) so an open parent head can be
/// refreshed. A no-op (empty) when `old_nested` is not a nested name.
pub fn promote_nested<N>(
    registry: &mut Registry<DataGraph>,
    timestamp: Duration,
    old_nested: &Name,
    new_name: &Name,
) -> Vec<Moved>
where
    N: Serialize + DeserializeOwned + gantz_core::Node + AsNamedRefMut,
{
    // The parent referencing the nested graph: the name with the last leaf
    // stripped (`A:1` -> `A`, `A:1:2` -> `A:1`).
    let Some(parent) = old_nested.parent() else {
        return Vec::new();
    };
    let (Some(new_graph), Some(parent_commit)) = (
        registry.named_commit(new_name).map(|c| c.graph),
        registry.head(&parent),
    ) else {
        return Vec::new();
    };

    // Repoint every parent reference (each a distinct instance) to the new name.
    let mut moves = Vec::new();
    moves.extend(commit_rewritten::<N>(
        registry,
        timestamp,
        &parent,
        parent_commit,
        |nr| {
            if nr.name() == old_nested {
                nr.rename(new_name.clone(), new_graph.into());
                true
            } else {
                false
            }
        },
    ));

    // Drop the orphaned nested name and its descendants (their content survives
    // as the new root graph copy).
    let orphans: Vec<Name> = registry
        .heads()
        .filter(|(n, _)| n.starts_with(old_nested))
        .map(|(n, _)| n.clone())
        .collect();
    for orphan in orphans {
        registry.remove_head(&orphan);
    }

    moves
}
