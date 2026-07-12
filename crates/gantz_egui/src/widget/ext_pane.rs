//! Application-supplied top-level panes (see [`ExtPane`]).

use crate::Responses;

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

    /// Render the pane's contents given the currently focused head (if any),
    /// returning any change payloads.
    fn ui(&mut self, focused: Option<&gantz_ca::Head>, ui: &mut egui::Ui) -> Responses;
}
