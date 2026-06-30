//! Reusable helpers for DSP node parameters: building a plyphon control param,
//! naming it uniquely within a synthdef, folding its smoothing lag into a content
//! address, and a combined value+lag inspector row.
//!
//! A DSP param's *value* lives in the node's VM state (path-keyed, like `number`),
//! so editing it does not churn the graph's content address; its *lag* lives in
//! the node weight, because lag is structural (it bakes a `LagControl` into the
//! synthdef and so respawns on change).

use gantz_core::node::{ExprCtx, ExprResult};
use plyphon::synthdef::Param;

/// The plyphon control [`Param`] named `name` with the given default value and
/// optional one-pole smoothing `lag` (seconds; `0.0` = a plain control).
pub fn plyphon_param(name: impl Into<String>, default: f32, lag: f32) -> Param {
    if lag > 0.0 {
        Param::lag(name, default, lag)
    } else {
        Param::control(name, default)
    }
}

/// Build a single-param DSP node's Steel `expr`.
///
/// If the node's *control input* at `control_ix` is connected, the incoming value
/// is written into the node's scalar VM state (`(set! state ...)`, guarded by
/// `number?` so a non-numeric value is ignored) - this is how a `number` or
/// `tick!`-driven chain drives the param at runtime. The expr always evaluates to
/// `output`: the node's placeholder dsp output (`"state"` for a source like
/// `~sine`, `"'()"` for a sink like `~out`). DSP nodes are otherwise Steel-inert;
/// the audio engine reads the same state slot and applies it via `set_control`.
pub fn control_input_expr(ctx: &ExprCtx<'_, '_>, control_ix: usize, output: &str) -> ExprResult {
    let expr = match ctx.inputs().get(control_ix) {
        Some(Some(val)) => {
            format!("(begin (if (number? {val}) (set! state {val}) void) {output})")
        }
        _ => format!("(begin {output})"),
    };
    gantz_core::node::parse_expr(&expr)
}

/// A synthdef parameter name unique to a node's parameter within a synthdef,
/// e.g. `"2/freq"` for the `freq` param of the node at path `[2]`.
pub fn param_name(path: &[usize], param: &str) -> String {
    let prefix = path
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("-");
    format!("{prefix}/{param}")
}

/// Fold a param's smoothing `lag` into a content-address hasher, but only when
/// non-zero - so an unsmoothed param leaves a node's address unchanged.
///
/// The lag is structural (it bakes a `LagControl` coefficient into the synthdef),
/// so unlike the param *value* (which lives in node state) it is part of identity.
pub fn cahash_lag(hasher: &mut gantz_ca::Hasher, lag: f32) {
    if lag != 0.0 {
        hasher.update(b"lag");
        hasher.update(&lag.to_le_bytes());
    }
}

/// One inspector row for a DSP param: the table key column is `name`; the value
/// column shows `value` (a caller-configured dialer) with the smoothing `lag` to
/// its right (e.g. `0.234   0.010 s lag`).
///
/// Returns `(value_changed, lag_changed)`. The caller writes the value to VM node
/// state (no `mark_changed` - it is not content-addressed) and the lag to the node
/// weight (`mark_changed` - it is structural).
pub fn param_row(
    body: &mut egui_extras::TableBody,
    name: &str,
    value: egui::DragValue<'_>,
    lag: &mut f32,
) -> (bool, bool) {
    let row_h = gantz_egui::widget::node_inspector::table_row_h(body.ui_mut());
    let mut value_changed = false;
    let mut lag_changed = false;
    body.row(row_h, |mut row| {
        row.col(|ui| {
            ui.label(name);
        });
        row.col(|ui| {
            ui.horizontal(|ui| {
                value_changed = ui.add(value).changed();
                let lag_dv = egui::DragValue::new(lag)
                    .range(0.0..=10.0)
                    .speed(0.001)
                    .fixed_decimals(3)
                    .suffix(" s lag");
                lag_changed = ui
                    .add(lag_dv)
                    .on_hover_text("one-pole smoothing time in seconds (0 = instant)")
                    .changed();
            });
        });
    });
    (value_changed, lag_changed)
}

/// One inspector row for a single value (no smoothing-lag field): the key column
/// is `name`, the value column is the caller-configured `value` dialer. Returns
/// whether it changed. Used for params that are themselves a duration (e.g.
/// `~lag`'s lag time), where there is no separate value-being-smoothed.
pub fn value_row(
    body: &mut egui_extras::TableBody,
    name: &str,
    value: egui::DragValue<'_>,
) -> bool {
    let row_h = gantz_egui::widget::node_inspector::table_row_h(body.ui_mut());
    let mut changed = false;
    body.row(row_h, |mut row| {
        row.col(|ui| {
            ui.label(name);
        });
        row.col(|ui| {
            changed = ui.add(value).changed();
        });
    });
    changed
}
