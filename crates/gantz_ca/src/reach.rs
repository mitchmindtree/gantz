//! The one reachability walk: liveness for prune, closure for export, and
//! want-lists for sync all derive from here.
//!
//! Edge rules:
//! - A commit contributes its parents and its graph.
//! - A graph contributes whatever `graph_out` reports: nested graph
//!   references and blob references. Only the application can see inside
//!   node payloads, so this is the one caller-supplied part.
//! - Blobs and section values are leaves.
//!
//! Roots are the entries of `Root`-liveness sections (the `heads` section
//! at minimum) plus any extra seeds the caller supplies.
//!
//! Section entry liveness is NOT part of [`LiveSet`]: it is a pure function
//! of the surviving content, so [`export`] and [`prune`] apply each
//! section's stored [`Liveness`] rule against the filtered
//! registry directly.

use crate::{
    BlobLiveness, CommitAddr, ContentAddr, GraphAddr, Liveness, Registry, SectionId, Value,
};
use std::collections::{BTreeMap, HashSet, VecDeque};

/// The outgoing content references of a single graph, as reported by the
/// application (it alone can interpret node payloads).
#[derive(Clone, Debug, Default)]
pub struct OutRefs {
    /// Nested graph references (e.g. `Ref` nodes).
    pub graphs: Vec<GraphAddr>,
    /// Blob references, tagged with their blob section.
    pub blobs: Vec<(SectionId, ContentAddr)>,
}

/// The live content of a registry: the closure of the roots (and any extra
/// seeds) over the edge rules above.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LiveSet {
    pub commits: HashSet<CommitAddr>,
    pub graphs: HashSet<GraphAddr>,
    pub blobs: BTreeMap<SectionId, HashSet<ContentAddr>>,
}

impl LiveSet {
    /// Whether the given blob is live.
    pub fn blob_live(&self, section: &str, addr: &ContentAddr) -> bool {
        self.blobs.get(section).is_some_and(|s| s.contains(addr))
    }
}

/// The live closure of `reg` from its `Root`-liveness sections plus the
/// given extra commit seeds.
///
/// `graph_out` maps a graph to its outgoing references (see [`OutRefs`]).
/// Applications wrap their node-payload introspection here (nested `Ref`
/// targets, blob references). Dangling seeds and references are tolerated
/// and simply not walked.
pub fn closure<G>(
    reg: &Registry<G>,
    extra_commits: impl IntoIterator<Item = CommitAddr>,
    graph_out: impl Fn(&G) -> OutRefs,
) -> LiveSet {
    let mut live = LiveSet::default();
    let mut commit_queue: VecDeque<CommitAddr> = VecDeque::new();
    let mut graph_queue: VecDeque<GraphAddr> = VecDeque::new();

    // Roots: every commit-valued entry of every Root-liveness section.
    for section in reg.sections().values() {
        if section.liveness != Liveness::Root {
            continue;
        }
        for value in section.entries.values() {
            if let Value::Commit(ca) = value {
                commit_queue.push_back(*ca);
            }
        }
    }
    commit_queue.extend(extra_commits);

    while let Some(ca) = commit_queue.pop_front() {
        let Some(commit) = reg.commits().get(&ca) else {
            continue;
        };
        if !live.commits.insert(ca) {
            continue;
        }
        commit_queue.extend(commit.parents());
        graph_queue.push_back(commit.graph);
        while let Some(ga) = graph_queue.pop_front() {
            let Some(graph) = reg.graph(&ga) else {
                continue;
            };
            if !live.graphs.insert(ga) {
                continue;
            }
            let out = graph_out(graph);
            graph_queue.extend(out.graphs);
            for (section, addr) in out.blobs {
                live.blobs.entry(section).or_default().insert(addr);
            }
        }
    }

    // Blobs referenced by live section entries (Value::Blob indirections).
    for section in reg.sections().values() {
        for (key, value) in &section.entries {
            if !entry_live(reg, section.liveness, key, &live) {
                continue;
            }
            if let Value::Blob(blob_section, addr) = value {
                live.blobs
                    .entry(blob_section.clone())
                    .or_default()
                    .insert(*addr);
            }
        }
    }

    // Pinned blob stores are live wholesale.
    for (id, store) in reg.blobs() {
        if store.liveness == BlobLiveness::Pinned {
            live.blobs
                .entry(id.clone())
                .or_default()
                .extend(store.entries.keys().copied());
        }
    }

    live
}

/// Export the live subset of the registry: content filtered by `live`,
/// section entries filtered by their stored liveness against the exported
/// content. Heads whose commit falls outside the export are dropped, and
/// their `WithName` metadata with them.
pub fn export<G: Clone>(reg: &Registry<G>, live: &LiveSet) -> Registry<G> {
    let mut exported = reg.clone();
    prune(&mut exported, live);
    exported
}

/// Prune the registry down to the live set: dead content is removed, heads
/// pointing outside the live set are dropped, each section's entries are
/// filtered by its stored liveness rule against the surviving state, and
/// invalid commit parents are detached. Emptied sections and blob stores
/// are removed.
pub fn prune<G>(reg: &mut Registry<G>, live: &LiveSet) {
    reg.retain_live(live);
}

/// Whether a section entry is live against the given registry + live set.
/// Used by [`closure`]'s blob-reference pass. `Root` entries are treated
/// conservatively as live (their commit values were the walk's seeds).
fn entry_live<G>(reg: &Registry<G>, liveness: Liveness, key: &crate::Key, live: &LiveSet) -> bool {
    use crate::Key;
    match liveness {
        Liveness::Pinned | Liveness::Root => true,
        Liveness::WithName => match key {
            Key::Name(name) => reg.head(name).is_some_and(|ca| live.commits.contains(&ca)),
            _ => false,
        },
        Liveness::WithCommit => match key {
            Key::Commit(ca) => live.commits.contains(ca),
            _ => false,
        },
        Liveness::WithGraph => match key {
            Key::Graph(ga) => live.graphs.contains(ga),
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentAddr, Key, MergePolicy, Name, registry::section_insert_datum};
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn graph_addr(n: u8) -> GraphAddr {
        GraphAddr::from(ContentAddr::from([n; 32]))
    }

    fn name(s: &str) -> Name {
        s.parse().unwrap()
    }

    /// A registry whose "graphs" are lists of nested graph addrs, so
    /// `graph_out` can be pure data.
    type TestGraph = Vec<GraphAddr>;

    fn out(g: &TestGraph) -> OutRefs {
        OutRefs {
            graphs: g.clone(),
            blobs: vec![],
        }
    }

    /// Add a graph of raw nested addrs under a synthetic address (the test
    /// graph type is plain data, so petgraph hashing does not apply).
    fn add_test_graph(reg: &mut Registry<TestGraph>, n: u8, nested: TestGraph) -> GraphAddr {
        let ga = graph_addr(n);
        reg.insert_graph_at(ga, nested);
        ga
    }

    #[test]
    fn closure_walks_heads_parents_and_nested_graphs() {
        let mut reg: Registry<TestGraph> = Registry::default();
        let nested = add_test_graph(&mut reg, 1, vec![]);
        let root_ga = add_test_graph(&mut reg, 2, vec![nested]);
        let c1 = reg.commit_graph(Duration::from_secs(1), None, root_ga, || unreachable!());
        let c2 = reg.commit_graph(Duration::from_secs(2), Some(c1), root_ga, || unreachable!());
        reg.set_head(name("alpha"), c2);
        let dead_ga = add_test_graph(&mut reg, 3, vec![graph_addr(99)]);
        let _dead = reg.commit_graph(Duration::from_secs(3), None, dead_ga, || unreachable!());
        let live = closure(&reg, [], out);
        assert!(live.commits.contains(&c1));
        assert!(live.commits.contains(&c2));
        assert!(live.graphs.contains(&root_ga));
        assert!(live.graphs.contains(&nested));
        assert!(!live.graphs.contains(&dead_ga));
        assert_eq!(live.commits.len(), 2);
    }

    #[test]
    fn graphs_stay_live_through_content_refs_without_commits() {
        let mut reg: Registry<TestGraph> = Registry::default();
        let nested = add_test_graph(&mut reg, 1, vec![]);
        let root_ga = add_test_graph(&mut reg, 2, vec![nested]);
        let c = reg.commit_graph(Duration::from_secs(1), None, root_ga, || unreachable!());
        reg.set_head(name("alpha"), c);
        let live = closure(&reg, [], out);
        prune(&mut reg, &live);
        assert!(reg.graph(&nested).is_some());
    }

    #[test]
    fn prune_drops_dead_content_and_sections_follow() {
        let mut reg: Registry<TestGraph> = Registry::default();
        let ga = add_test_graph(&mut reg, 1, vec![]);
        let live_c = reg.commit_graph(Duration::from_secs(1), None, ga, || unreachable!());
        let dead_c = reg.commit_graph(Duration::from_secs(2), None, ga, || unreachable!());
        reg.set_head(name("alpha"), live_c);
        for ca in [live_c, dead_c] {
            section_insert_datum(
                &mut reg,
                "test.view",
                MergePolicy::KeepExisting,
                Liveness::WithCommit,
                Key::Commit(ca),
                &"view".to_string(),
            )
            .unwrap();
        }
        let live = closure(&reg, [], out);
        prune(&mut reg, &live);
        assert!(reg.commits().contains_key(&live_c));
        assert!(!reg.commits().contains_key(&dead_c));
        let section = reg.section("test.view").unwrap();
        assert!(section.entries.contains_key(&Key::Commit(live_c)));
        assert!(!section.entries.contains_key(&Key::Commit(dead_c)));
    }

    #[test]
    fn prune_detaches_pruned_parents() {
        let mut reg: Registry<TestGraph> = Registry::default();
        let ga = add_test_graph(&mut reg, 1, vec![]);
        let old = reg.commit_graph(Duration::from_secs(1), None, ga, || unreachable!());
        let tip = reg.commit_graph(Duration::from_secs(2), Some(old), ga, || unreachable!());
        reg.set_head(name("alpha"), tip);
        let live = LiveSet {
            commits: [tip].into_iter().collect(),
            graphs: [ga].into_iter().collect(),
            blobs: BTreeMap::new(),
        };
        prune(&mut reg, &live);
        assert!(!reg.commits().contains_key(&old));
        assert_eq!(reg.commits()[&tip].parent, None);
    }

    #[test]
    fn blob_liveness_follows_content_refs() {
        let mut reg: Registry<TestGraph> = Registry::default();
        let used = reg.add_blob("dsp.buffer", BlobLiveness::ContentReferenced, &b"used"[..]);
        let unused = reg.add_blob(
            "dsp.buffer",
            BlobLiveness::ContentReferenced,
            &b"unused"[..],
        );
        let ga = add_test_graph(&mut reg, 1, vec![]);
        let c = reg.commit_graph(Duration::from_secs(1), None, ga, || unreachable!());
        reg.set_head(name("alpha"), c);
        let graph_out = |_: &TestGraph| OutRefs {
            graphs: vec![],
            blobs: vec![("dsp.buffer".to_string(), used)],
        };
        let live = closure(&reg, [], graph_out);
        assert!(live.blob_live("dsp.buffer", &used));
        assert!(!live.blob_live("dsp.buffer", &unused));
        prune(&mut reg, &live);
        assert!(reg.blob("dsp.buffer", &used).is_some());
        assert!(reg.blob("dsp.buffer", &unused).is_none());
    }

    #[test]
    fn blob_liveness_follows_section_values() {
        let mut reg: Registry<TestGraph> = Registry::default();
        let pinned_by_section =
            reg.add_blob("ui.assets", BlobLiveness::SectionReferenced, &b"icon"[..]);
        let orphan = reg.add_blob("ui.assets", BlobLiveness::SectionReferenced, &b"old"[..]);
        let ga = add_test_graph(&mut reg, 1, vec![]);
        let c = reg.commit_graph(Duration::from_secs(1), None, ga, || unreachable!());
        reg.set_head(name("alpha"), c);
        reg.set_section_value(
            "test.icons",
            MergePolicy::KeepExisting,
            Liveness::WithName,
            Key::Name(name("alpha")),
            Value::Blob("ui.assets".to_string(), pinned_by_section),
        );
        let live = closure(&reg, [], out);
        assert!(live.blob_live("ui.assets", &pinned_by_section));
        assert!(!live.blob_live("ui.assets", &orphan));
    }

    #[test]
    fn export_filters_heads_to_exported_commits() {
        let mut reg: Registry<TestGraph> = Registry::default();
        let ga = add_test_graph(&mut reg, 1, vec![]);
        let ca = reg.commit_graph(Duration::from_secs(1), None, ga, || unreachable!());
        let cb = reg.commit_graph(Duration::from_secs(2), None, ga, || unreachable!());
        reg.set_head(name("alpha"), ca);
        reg.set_head(name("beta"), cb);
        // Alpha's closure only.
        let live = LiveSet {
            commits: [ca].into_iter().collect(),
            graphs: [ga].into_iter().collect(),
            blobs: BTreeMap::new(),
        };
        let exported = export(&reg, &live);
        assert_eq!(exported.head(&name("alpha")), Some(ca));
        assert_eq!(exported.head(&name("beta")), None);
        assert!(exported.commits().contains_key(&ca));
        assert!(!exported.commits().contains_key(&cb));
        // The source registry is untouched.
        assert_eq!(reg.head(&name("beta")), Some(cb));
    }

    #[test]
    fn export_keeps_with_name_metadata_for_exported_heads_only() {
        let mut reg: Registry<TestGraph> = Registry::default();
        let ga = add_test_graph(&mut reg, 1, vec![]);
        let ca = reg.commit_graph(Duration::from_secs(1), None, ga, || unreachable!());
        let cb = reg.commit_graph(Duration::from_secs(2), None, ga, || unreachable!());
        reg.set_head(name("alpha"), ca);
        reg.set_head(name("beta"), cb);
        for n in ["alpha", "beta"] {
            section_insert_datum(
                &mut reg,
                "test.description",
                MergePolicy::KeepExisting,
                Liveness::WithName,
                Key::Name(name(n)),
                &format!("doc for {n}"),
            )
            .unwrap();
        }
        let live = LiveSet {
            commits: [ca].into_iter().collect(),
            graphs: [ga].into_iter().collect(),
            blobs: BTreeMap::new(),
        };
        let exported = export(&reg, &live);
        let section = exported.section("test.description").unwrap();
        assert!(section.entries.contains_key(&Key::Name(name("alpha"))));
        assert!(!section.entries.contains_key(&Key::Name(name("beta"))));
    }

    #[test]
    fn unknown_section_survives_export_and_merge() {
        let mut reg: Registry<TestGraph> = Registry::default();
        let ga = add_test_graph(&mut reg, 1, vec![]);
        let ca = reg.commit_graph(Duration::from_secs(1), None, ga, || unreachable!());
        reg.set_head(name("alpha"), ca);
        section_insert_datum(
            &mut reg,
            "laser.palette",
            MergePolicy::KeepExisting,
            Liveness::Pinned,
            Key::Name(name("show")),
            &vec![255u8, 0, 128],
        )
        .unwrap();
        let live = closure(&reg, [], out);
        let exported = export(&reg, &live);
        assert!(exported.section("laser.palette").is_some());
        let mut other: Registry<TestGraph> = Registry::default();
        other.merge(exported);
        let section = other.section("laser.palette").unwrap();
        assert_eq!(section.entries.len(), 1);
        assert_eq!(section.liveness, Liveness::Pinned);
    }

    #[test]
    fn export_of_nothing_is_empty() {
        let mut reg: Registry<TestGraph> = Registry::default();
        let ga = add_test_graph(&mut reg, 1, vec![]);
        let ca = reg.commit_graph(Duration::from_secs(1), None, ga, || unreachable!());
        reg.set_head(name("alpha"), ca);
        let exported = export(&reg, &LiveSet::default());
        assert!(exported.commits().is_empty());
        assert!(exported.graphs().is_empty());
        assert_eq!(exported.head(&name("alpha")), None);
        assert!(exported.sections().is_empty());
    }
}
