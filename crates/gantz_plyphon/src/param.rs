//! A reusable settable DSP parameter ([`DspParam`]) - a value plus an optional
//! one-pole smoothing lag - and its inspector row.

use std::fmt;
use std::hash::{Hash, Hasher};

use plyphon::synthdef::Param;
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserializer, Serialize, Serializer};

/// A settable DSP parameter: a `value` plus an optional one-pole smoothing `lag`
/// (seconds; `0.0` = instant).
///
/// Lives in a node's weight. The `value` is edited via a dialer and `set_control`'d
/// on the running synth (no respawn, so phase is preserved). The `lag` is
/// *structural* - it is baked into the compiled synthdef, so changing it respawns.
/// Per-parameter, so a node can smooth its gain but leave its frequency instant.
///
/// Serializes as a bare number when unsmoothed (`lag == 0`), and as
/// `(value, lag)` otherwise - so a default param is byte-compatible with a plain
/// `f32` field.
#[derive(Clone, Copy, Debug)]
pub struct DspParam {
    /// The current value (the plyphon param's default).
    pub value: f32,
    /// One-pole smoothing time in seconds (`0.0` = no smoothing).
    pub lag: f32,
}

impl DspParam {
    /// A parameter with the given value and no smoothing.
    pub fn new(value: f32) -> Self {
        DspParam { value, lag: 0.0 }
    }

    /// A parameter with the given value and one-pole smoothing `lag` (seconds).
    pub fn lagged(value: f32, lag: f32) -> Self {
        DspParam { value, lag }
    }

    /// The corresponding plyphon [`Param`] named `name`: a `LagControl` when
    /// `lag > 0`, else a plain control.
    pub fn to_plyphon(&self, name: impl Into<String>) -> Param {
        if self.lag > 0.0 {
            Param::lag(name, self.value, self.lag)
        } else {
            Param::control(name, self.value)
        }
    }

    /// Fold this parameter into a content-address [`gantz_ca::Hasher`].
    ///
    /// The `lag` is folded only when non-zero, so an unsmoothed param hashes to
    /// just its value bytes - keeping a default node's address stable.
    pub fn cahash(&self, hasher: &mut gantz_ca::Hasher) {
        hasher.update(&self.value.to_le_bytes());
        if self.lag != 0.0 {
            hasher.update(b"lag");
            hasher.update(&self.lag.to_le_bytes());
        }
    }
}

impl PartialEq for DspParam {
    fn eq(&self, other: &Self) -> bool {
        self.value.to_bits() == other.value.to_bits() && self.lag.to_bits() == other.lag.to_bits()
    }
}

impl Eq for DspParam {}

impl Hash for DspParam {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash(&self.value.to_bits(), state);
        Hash::hash(&self.lag.to_bits(), state);
    }
}

impl Serialize for DspParam {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.lag == 0.0 {
            // Compact + back-compatible: an unsmoothed param is just its value.
            self.value.serialize(s)
        } else {
            let mut st = s.serialize_struct("DspParam", 2)?;
            st.serialize_field("value", &self.value)?;
            st.serialize_field("lag", &self.lag)?;
            st.end()
        }
    }
}

impl<'de> serde::Deserialize<'de> for DspParam {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Accept either a bare number (unsmoothed) or a `{value, lag}` map, via
        // the self-describing `deserialize_any` (works for RON and the `.gantz`
        // Datum codec).
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = DspParam;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a number or a {value, lag} map")
            }
            fn visit_f64<E>(self, v: f64) -> Result<DspParam, E> {
                Ok(DspParam::new(v as f32))
            }
            fn visit_f32<E>(self, v: f32) -> Result<DspParam, E> {
                Ok(DspParam::new(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<DspParam, E> {
                Ok(DspParam::new(v as f32))
            }
            fn visit_u64<E>(self, v: u64) -> Result<DspParam, E> {
                Ok(DspParam::new(v as f32))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<DspParam, A::Error> {
                let (mut value, mut lag) = (None, 0.0f32);
                while let Some(k) = map.next_key::<String>()? {
                    match k.as_str() {
                        "value" => value = Some(map.next_value()?),
                        "lag" => lag = map.next_value()?,
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                let value = value.ok_or_else(|| de::Error::missing_field("value"))?;
                Ok(DspParam { value, lag })
            }
        }
        d.deserialize_any(V)
    }
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

/// Add an inspector row for a [`DspParam`]'s smoothing lag (seconds; `0` =
/// instant). Returns whether it changed. The param's *value* is edited in the
/// node body, not here.
pub fn lag_row(body: &mut egui_extras::TableBody, label: &str, param: &mut DspParam) -> bool {
    let row_h = gantz_egui::widget::node_inspector::table_row_h(body.ui_mut());
    let mut changed = false;
    body.row(row_h, |mut row| {
        row.col(|ui| {
            ui.label(label)
                .on_hover_text("one-pole smoothing time in seconds (0 = instant)");
        });
        row.col(|ui| {
            let dv = egui::DragValue::new(&mut param.lag)
                .range(0.0..=10.0)
                .speed(0.001)
                .suffix(" s");
            if ui.add(dv).changed() {
                changed = true;
            }
        });
    });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip_and_back_compat() {
        // Unsmoothed round-trips, and the old bare-f32 form still deserializes.
        let p = DspParam::new(220.0);
        let s = ron::to_string(&p).expect("ser");
        assert_eq!(ron::from_str::<DspParam>(&s).expect("de"), p);
        assert_eq!(
            ron::from_str::<DspParam>("330.0").expect("bare de"),
            DspParam::new(330.0),
            "an old bare-f32 param must still deserialize",
        );
        // Smoothed round-trips as a struct.
        let q = DspParam::lagged(0.2, 0.01);
        let s = ron::to_string(&q).expect("ser");
        assert_eq!(ron::from_str::<DspParam>(&s).expect("de"), q);
    }
}
