//! Base nodes - pre-composed graphs that ship with the binary.
//!
//! Base nodes are named graphs authored as `.gantz` files and embedded at
//! compile time via `include_bytes!`. Each domain contributes its file as a
//! [`BaseSource`] pushed into [`BaseSources`] from its plugin's `build`
//! (`GantzEguiPlugin` contributes the core `gantz_base` source). On every
//! startup, [`load`] deserializes each source in order and merges it into
//! the user's registry so base nodes are always available. Because the merge
//! replaces existing names, base nodes are authoritative - they reset to
//! their original form on each launch. Users who want to customize a base
//! node should duplicate it under a new name.
//!
//! A source may reference names another source defines: loading runs to a
//! fixpoint, parsing each source seeded with the names loaded so far (see
//! [`load`]), so e.g. a domain's base graph can compose the core source's
//! graphs. Its file writes those refs by name without embedding the foreign
//! graphs (see [`export_to_file`]).
//!
//! The set of base node names is tracked in [`BaseNames`] so the UI can
//! distinguish them (e.g. `[base]` prefix, no delete button), and each
//! name's owning source in [`BaseNameSources`] so `update-base` can write
//! every source back to its own file and demo reset can re-parse the right
//! source.

use bevy_ecs::prelude::*;
use bevy_gantz::reg::Registry;
use bevy_log as log;
use gantz_core::node::graph::Graph;
use std::collections::HashMap;

use crate::BaseNames;

/// Fixed timestamp used to stamp the base's hand-authored (uncommitted) graphs.
///
/// Every base source is parsed at startup *and* again on demo reset; both must
/// agree on the synthesized commit addresses, otherwise a reset demo's `ref`s
/// point at commits that are absent from the already-loaded registry (its
/// primitives were stamped at startup). A constant makes those addresses
/// reproducible.
pub const BASE_TIMESTAMP: gantz_ca::Timestamp = std::time::Duration::ZERO;

/// One domain's baked-in base `.gantz` export.
pub struct BaseSource {
    /// Identifies the source (e.g. `"gantz"`, `"plyphon"`) in logs, name
    /// attribution ([`BaseNameSources`]) and `update-base` write-back routing.
    pub name: &'static str,
    /// The `.gantz` bytes (an `include_bytes!` of the domain's base file).
    pub bytes: &'static [u8],
}

/// The base sources to load, in load order.
///
/// Domain plugins contribute their source via `get_resource_or_init` + push
/// from `Plugin::build`, so plugin order does not matter for correctness
/// (name collisions across sources are resolved last-wins, loudly - see
/// [`load`]).
#[derive(Default, Resource)]
pub struct BaseSources(pub Vec<BaseSource>);

/// Which source each base name came from (name to [`BaseSource::name`]),
/// recorded by [`load`].
///
/// `update-base` uses it to write each source's names back to that source's
/// own file, and demo reset uses it to re-parse the owning source.
#[derive(Default, Resource)]
pub struct BaseNameSources(pub HashMap<String, &'static str>);

/// Startup system that deserializes each embedded base source and merges it
/// into the registry, populating [`BaseNames`] and [`BaseNameSources`].
///
/// A source may reference names another source defines, so loading runs to a
/// fixpoint: each round parses every still-pending source seeded with the
/// names loaded so far, deferring sources whose references do not resolve
/// yet. Push order therefore does not matter - sources load in dependency
/// order. A source whose references never resolve (or that fails to parse
/// outright) is logged and dropped.
pub fn load<N>(
    sources: Res<BaseSources>,
    mut registry: ResMut<Registry<N>>,
    mut base_names: ResMut<BaseNames>,
    mut name_sources: ResMut<BaseNameSources>,
    mut views: ResMut<crate::Views>,
    mut demos: ResMut<crate::Demos>,
) where
    N: 'static
        + serde::Serialize
        + serde::de::DeserializeOwned
        + gantz_ca::CaHash
        + gantz_format::NodeSugar
        + Send
        + Sync,
{
    let mut pending: Vec<&BaseSource> = sources.0.iter().collect();
    loop {
        let mut deferred: Vec<&BaseSource> = Vec::new();
        for source in pending.iter().copied() {
            let export: gantz_egui::export::Export<Graph<N>> =
                match gantz_egui::export::parse_export_seeded_at(
                    source.bytes,
                    BASE_TIMESTAMP,
                    &base_names.0,
                ) {
                    Ok(e) => e,
                    // An unresolved reference may resolve once another
                    // source loads - retry next round.
                    Err(gantz_egui::export::ParseExportError::Format(e))
                        if matches!(e.kind, gantz_format::ErrorKind::MissingDependency(_)) =>
                    {
                        deferred.push(source);
                        continue;
                    }
                    Err(e) => {
                        log::error!("base source `{}`: {e}", source.name);
                        continue;
                    }
                };
            for name in export.registry.names().keys() {
                if let Some(prev) = name_sources.0.get(name.as_str()) {
                    log::warn!(
                        "base source `{}` redefines `{name}` from source `{prev}` \
                         (last source wins)",
                        source.name,
                    );
                }
                name_sources.0.insert(name.clone(), source.name);
            }
            base_names.0.extend(export.registry.names().clone());
            // NOTE: the merge's `names_replaced` is deliberately not logged -
            // base names replacing a user's persisted edits on launch is the
            // by-design authoritative reset, not a collision. Source-vs-source
            // collisions are the ones worth warning about, caught above.
            registry.merge(export.registry);
            views.0.extend(export.views);
            demos.0.extend(export.demos);
        }
        // Done, or stuck: no deferred source can make progress once a full
        // round loads nothing new.
        if deferred.is_empty() {
            break;
        }
        if deferred.len() == pending.len() {
            for source in deferred {
                if let Err(err) = gantz_egui::export::parse_export_seeded_at::<N>(
                    source.bytes,
                    BASE_TIMESTAMP,
                    &base_names.0,
                ) {
                    log::error!(
                        "base source `{}` has unresolvable references: {err}",
                        source.name,
                    );
                }
            }
            break;
        }
        pending = deferred;
    }
}

/// Paths to write each base source back to (a [`BaseSource::name`] to file
/// path map), plus the source that receives names with no recorded
/// attribution (graphs created during the session).
///
/// Used by [`export_to_file`]. The paths typically point at each source
/// crate's `base.gantz` file so that edits land back in the repo. This lives
/// in the developer tool's configuration, not on [`BaseSource`]: shipped
/// binaries must not bake dev-tree write paths.
#[derive(Resource)]
pub struct ExportPaths {
    /// Where each source's names are written ([`BaseSource::name`] to path).
    pub paths: HashMap<&'static str, &'static str>,
    /// The source that receives unattributed (session-created) names.
    pub default_source: &'static str,
}

/// System that exports every named graph back to its owning source's file
/// (see [`ExportPaths`] and [`BaseNameSources`]).
///
/// Intended for the `update-base` developer binary. Pair with
/// `DebouncedInputEvent` so it runs on save.
pub fn export_to_file<N>(
    paths: Res<ExportPaths>,
    name_sources: Res<BaseNameSources>,
    registry: Res<Registry<N>>,
    views: Res<crate::Views>,
    demos: Res<crate::Demos>,
) where
    N: 'static
        + serde::Serialize
        + serde::de::DeserializeOwned
        + gantz_core::Node
        + Clone
        + gantz_format::NodeSugar
        + Send
        + Sync,
{
    let partitioned = partition_names(registry.names().keys(), &name_sources, paths.default_source);
    for (source, names) in &partitioned {
        let Some(path) = paths.paths.get(source) else {
            log::warn!(
                "export_to_file: no path configured for base source `{source}` \
                 ({} names skipped)",
                names.len(),
            );
            continue;
        };
        // Exactly this source's names, with refs into other sources written
        // by name only (no transitive closure) - loading resolves them via
        // the seeded parse.
        match gantz_egui::export::export_names_sexpr_named(&registry, &views, &demos.0, names) {
            Ok(text) => {
                if let Err(e) = std::fs::write(path, text) {
                    log::error!("export_to_file: failed to write {path}: {e}");
                }
            }
            Err(e) => log::error!("export_to_file: failed to serialize `{source}`: {e}"),
        }
    }
}

/// Serialize all named graphs to `.gantz` text in the inline-name format.
///
/// This is the base writer for the `update-base` developer workflow, so it uses
/// [`gantz_egui::export::export_heads_sexpr_named`]: graphs named inline, no
/// commits/names tables, references by name - keeping `base.gantz` hand-editable
/// and free of churning addresses. (Other export paths keep the default
/// address-based format.) Returns `None` on serialization failure.
pub fn export_all_named<N>(
    registry: &Registry<N>,
    builtins: &bevy_gantz::BuiltinNodes<N>,
    views: &crate::Views,
    demos: &crate::Demos,
) -> Option<String>
where
    N: 'static
        + serde::Serialize
        + serde::de::DeserializeOwned
        + gantz_core::Node
        + Clone
        + gantz_format::NodeSugar
        + Send
        + Sync,
{
    let node_reg = crate::registry_ref(registry, builtins, demos);
    let get_node = |ca: &gantz_ca::ContentAddr| node_reg.node(ca);

    let named_heads: Vec<gantz_ca::Head> = registry
        .names()
        .keys()
        .map(|name| gantz_ca::Head::Branch(name.clone()))
        .collect();

    gantz_egui::export::export_heads_sexpr_named(
        &get_node,
        registry,
        views,
        &demos.0,
        named_heads.iter(),
    )
    .ok()
}

/// Partition base names by their owning source for per-source write-back.
///
/// A nested name (`parent:child`) with no recorded source follows its
/// parent's attribution, recursing to the outermost prefix - a nested graph
/// belongs in the same file as the graph that nests it. Other unrecorded
/// names (e.g. graphs created during an `update-base` session) are attributed
/// to `default_source`.
pub fn partition_names<'a>(
    names: impl IntoIterator<Item = &'a String>,
    name_sources: &BaseNameSources,
    default_source: &'static str,
) -> std::collections::BTreeMap<&'static str, Vec<String>> {
    fn source_of(
        name: &str,
        name_sources: &BaseNameSources,
        default_source: &'static str,
    ) -> &'static str {
        if let Some(&source) = name_sources.0.get(name) {
            return source;
        }
        match name.rsplit_once(gantz_egui::node::NESTED_SEP) {
            Some((parent, _)) => source_of(parent, name_sources, default_source),
            None => default_source,
        }
    }
    let mut partitioned = std::collections::BTreeMap::<&'static str, Vec<String>>::new();
    for name in names {
        let source = source_of(name, name_sources, default_source);
        partitioned.entry(source).or_default().push(name.clone());
    }
    partitioned
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Attributed names route to their source, unattributed names to the
    /// default source.
    #[test]
    fn partition_names_routes_by_attribution() {
        let mut name_sources = BaseNameSources::default();
        name_sources.0.insert("add".to_string(), "gantz");
        name_sources.0.insert("demo-sine".to_string(), "plyphon");
        let names = [
            "add".to_string(),
            "demo-sine".to_string(),
            "new".to_string(),
        ];
        let partitioned = partition_names(names.iter(), &name_sources, "gantz");
        assert_eq!(
            partitioned.get("gantz"),
            Some(&vec!["add".to_string(), "new".to_string()]),
        );
        assert_eq!(
            partitioned.get("plyphon"),
            Some(&vec!["demo-sine".to_string()])
        );
    }

    /// An unrecorded nested name follows its parent's attribution, however
    /// deep, before falling back to the default.
    #[test]
    fn partition_names_nested_follow_their_parent() {
        let mut name_sources = BaseNameSources::default();
        name_sources.0.insert("demo-sine".to_string(), "plyphon");
        let names = [
            "demo-sine:child".to_string(),
            "demo-sine:child:grandchild".to_string(),
            "orphan:child".to_string(),
        ];
        let partitioned = partition_names(names.iter(), &name_sources, "gantz");
        assert_eq!(
            partitioned.get("plyphon"),
            Some(&vec![
                "demo-sine:child".to_string(),
                "demo-sine:child:grandchild".to_string(),
            ]),
        );
        assert_eq!(
            partitioned.get("gantz"),
            Some(&vec!["orphan:child".to_string()]),
        );
    }
}
