//! Application-supplied top-level panes (see [`ExtPane`]).

use crate::Responses;
use gantz_core::node;

/// An application-supplied top-level pane - the pane analogue of
/// [`SettingsTab`][super::SettingsTab].
///
/// Domains contribute their own panes by supplying implementations to the
/// [`Gantz`][super::Gantz] widget via
/// [`Gantz::ext_panes`][super::Gantz::ext_panes]. A pane typically holds a
/// per-frame snapshot of its domain's data, and reports changes by pushing
/// typed payloads into the returned [`Responses`] for the host to apply.
///
/// A supplied pane's tile is inserted into the tray on first sight of its
/// [`key`][Self::key] and persists in the tile tree from then on. While no
/// provider supplies the key (e.g. the owning domain is disabled), the tile
/// renders a placeholder. Visibility defaults to hidden, toggled via
/// Settings -> Panes like the built-in tray panes.
pub trait ExtPane {
    /// The pane's stable identity: persisted in the tile tree and keying its
    /// pop-out window geometry. Unique among the supplied panes and stable
    /// across sessions.
    fn key(&self) -> &str;

    /// The tab label. Suffixed with the focused head in the tab title, like
    /// the built-in Steel pane.
    fn title(&self) -> &str;

    /// Hover text for the pane's Settings -> Panes visibility checkbox.
    fn description(&self) -> &str {
        ""
    }

    /// Render the pane's contents, returning any change payloads.
    fn ui(&mut self, cx: ExtPaneCtx, ui: &mut egui::Ui) -> Responses;
}

/// The context handed to [`ExtPane::ui`] - what the widget knows about the
/// focused head that a pane might want to follow (the built-in Steel pane's
/// inputs, roughly).
///
/// `#[non_exhaustive]` so future context reaches panes without breaking
/// implementors: only the widget constructs one.
#[non_exhaustive]
pub struct ExtPaneCtx<'a> {
    /// The currently focused head, if any.
    pub focused: Option<&'a gantz_ca::Head>,
    /// The focused head's selected nodes (root-level indices), sorted.
    pub selection: &'a [node::Id],
}

/// One supplied extension pane's identity and labels, for the pane-visibility
/// checkbox UIs (Settings -> Panes and the graph-area context menu's "panes"
/// submenu - see [`panes_config`][super::panes_config()]).
#[derive(Clone, Debug)]
pub struct ExtPaneEntry {
    /// The pane's stable identity ([`ExtPane::key`]).
    pub key: String,
    /// The checkbox label ([`ExtPane::title`]).
    pub title: String,
    /// The checkbox hover text ([`ExtPane::description`]).
    pub description: String,
}
