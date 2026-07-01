//! `.gantz` keyword sugar for the DSP node set.
//!
//! [`PlyphonSugar`] provides the human-friendly keywords for this crate's nodes:
//! bare `~sine`/`~out`/`~lag`/`~tap`, with optional `(~sine #:freq-lag s)`,
//! `(~out #:gain-lag s)` and `(~tap #:size n)` forms carrying the (structural)
//! smoothing lag / ring length. The `freq`/`gain`/`dur` param *values* live in VM
//! state (not the node weight), so they are not serialized and never appear here.
//! Compose it with [`gantz_format::CoreSugar`] (and the other crates' sugars) via
//! [`gantz_format::Sugars`].

use gantz_format::{Datum, FormatError, Sugar, SugarArgs, node_datum};

/// Keyword sugar for the plyphon DSP nodes ([`Sine`](crate::Sine),
/// [`Out`](crate::Out), [`Lag`](crate::Lag), [`Tap`](crate::Tap)).
#[derive(Clone, Copy, Debug, Default)]
pub struct PlyphonSugar;

/// Sugar keyword -> typetag tag.
const KEYWORD_TAG: &[(&str, &str)] = &[
    ("~sine", "Sine"),
    ("~out", "Out"),
    ("~lag", "Lag"),
    ("~tap", "Tap"),
];

/// The typetag tag for a sugar keyword.
fn tag_for_keyword(kw: &str) -> Option<&'static str> {
    KEYWORD_TAG
        .iter()
        .find(|(k, _)| *k == kw)
        .map(|&(_, tag)| tag)
}

/// The sugar keyword for a typetag tag, if one exists.
fn keyword_for_tag(tag: &str) -> Option<&'static str> {
    KEYWORD_TAG
        .iter()
        .find(|(_, t)| *t == tag)
        .map(|&(kw, _)| kw)
}

impl Sugar for PlyphonSugar {
    fn read_spec(&self, head: &str, args: SugarArgs<'_>) -> Result<Option<Datum>, FormatError> {
        let datum = match head {
            "~sine" => lag_spec("Sine", "freq_lag", "freq-lag", args)?,
            "~out" => lag_spec("Out", "gain_lag", "gain-lag", args)?,
            "~lag" => node_datum("Lag", vec![]),
            "~tap" => size_spec(args)?,
            _ => return Ok(None),
        };
        Ok(Some(datum))
    }

    fn read_bare(&self, keyword: &str) -> Option<Datum> {
        tag_for_keyword(keyword).map(|tag| node_datum(tag, vec![]))
    }

    fn write_spec(&self, tag: &str, node: &Datum) -> Option<String> {
        match tag {
            "Sine" => Some(write_lag(
                "~sine",
                "freq_lag",
                "freq-lag",
                crate::Sine::DEFAULT_FREQ_LAG,
                node,
            )),
            "Out" => Some(write_lag(
                "~out",
                "gain_lag",
                "gain-lag",
                crate::Out::DEFAULT_GAIN_LAG,
                node,
            )),
            "Tap" => Some(write_size(node)),
            other => keyword_for_tag(other).map(str::to_string),
        }
    }

    fn keyword_for_tag(&self, tag: &str) -> Option<&str> {
        keyword_for_tag(tag)
    }
}

/// Read a `(<head> [#:<keyword> s])` form into a node datum tagged `tag`, carrying
/// the `field` lag only when the keyword is present (so a bare form stays bare).
fn lag_spec(
    tag: &str,
    field: &str,
    keyword: &str,
    args: SugarArgs<'_>,
) -> Result<Datum, FormatError> {
    let mut fields = Vec::new();
    if let Some(lag) = args.keyword_f64(keyword)? {
        fields.push((field, Datum::F64(lag)));
    }
    Ok(node_datum(tag, fields))
}

/// Write a node carrying a smoothing `field` lag: the bare keyword `kw` when the lag
/// is at `default`, else `(<kw> #:<keyword> <lag>)`. The stored value is an `f32`
/// widened to `f64`; comparing and formatting it back as `f32` keeps the form exact
/// and tidy (e.g. `0.01`, not `0.00999999977648258`).
fn write_lag(kw: &str, field: &str, keyword: &str, default: f32, node: &Datum) -> String {
    match node.get(field).and_then(Datum::as_f64) {
        Some(lag) if lag as f32 != default => format!("({kw} #:{keyword} {})", lag as f32),
        _ => kw.to_string(),
    }
}

/// Read a `(~tap [#:size n])` form into a `Tap` node datum, carrying the ring
/// `size` only when the keyword is present (so a bare form stays bare).
fn size_spec(args: SugarArgs<'_>) -> Result<Datum, FormatError> {
    let mut fields = Vec::new();
    if let Some(size) = args.keyword_int("size")? {
        fields.push(("size", Datum::U64(size.max(1) as u64)));
    }
    Ok(node_datum("Tap", fields))
}

/// Write a `Tap`: the bare `~tap` when the ring `size` is at its default, else
/// `(~tap #:size n)`.
fn write_size(node: &Datum) -> String {
    match node.get("size").and_then(Datum::as_i64) {
        Some(size) if size != crate::Tap::DEFAULT_SIZE as i64 => {
            format!("(~tap #:size {size})")
        }
        _ => "~tap".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gantz_format::sexpr;

    /// Read a single sugar form's text through `PlyphonSugar`, as the format does.
    fn read_spec(text: &str) -> Option<Datum> {
        let exprs = sexpr::read(text).expect("read");
        let args = sexpr::list_args(&exprs[0]).expect("list");
        let head = sexpr::as_symbol(&args[0]).expect("head");
        PlyphonSugar
            .read_spec(&head, SugarArgs::new(&args[1..], text))
            .expect("read_spec")
    }

    #[test]
    fn sine_round_trips() {
        let s = PlyphonSugar;
        // A default `~sine` stays bare, read as a bare keyword or an empty spec.
        let bare = s.read_bare("~sine").expect("bare");
        assert_eq!(bare.get("type").and_then(Datum::as_str), Some("Sine"));
        assert_eq!(s.write_spec("Sine", &bare).as_deref(), Some("~sine"));
        let empty = read_spec("(~sine)").expect("empty");
        assert_eq!(s.write_spec("Sine", &empty).as_deref(), Some("~sine"));
        // A non-default freq lag round-trips.
        let lagged = read_spec("(~sine #:freq-lag 0.5)").expect("lagged");
        assert_eq!(lagged.get("freq_lag").and_then(Datum::as_f64), Some(0.5));
        assert_eq!(
            s.write_spec("Sine", &lagged).as_deref(),
            Some("(~sine #:freq-lag 0.5)"),
        );
    }

    #[test]
    fn out_round_trips() {
        let s = PlyphonSugar;
        // A default `~out` stays bare - including a Datum carrying the default lag
        // (the f32 round-trips through f64 without tripping the default check).
        let bare = s.read_bare("~out").expect("bare");
        assert_eq!(s.write_spec("Out", &bare).as_deref(), Some("~out"));
        let defaulted = node_datum(
            "Out",
            vec![(
                "gain_lag",
                Datum::F64(f64::from(crate::Out::DEFAULT_GAIN_LAG)),
            )],
        );
        assert_eq!(s.write_spec("Out", &defaulted).as_deref(), Some("~out"));
        // A non-default gain lag round-trips, tidily.
        let lagged = read_spec("(~out #:gain-lag 0.02)").expect("lagged");
        assert_eq!(
            s.write_spec("Out", &lagged).as_deref(),
            Some("(~out #:gain-lag 0.02)"),
        );
    }

    #[test]
    fn lag_round_trips() {
        let s = PlyphonSugar;
        let bare = s.read_bare("~lag").expect("bare");
        assert_eq!(bare.get("type").and_then(Datum::as_str), Some("Lag"));
        assert_eq!(s.write_spec("Lag", &bare).as_deref(), Some("~lag"));
        let spec = read_spec("(~lag)").expect("spec");
        assert_eq!(spec.get("type").and_then(Datum::as_str), Some("Lag"));
        assert_eq!(s.write_spec("Lag", &spec).as_deref(), Some("~lag"));
    }

    #[test]
    fn tap_round_trips() {
        let s = PlyphonSugar;
        // A default `~tap` stays bare (read as a bare keyword or an empty spec).
        let bare = s.read_bare("~tap").expect("bare");
        assert_eq!(bare.get("type").and_then(Datum::as_str), Some("Tap"));
        assert_eq!(s.write_spec("Tap", &bare).as_deref(), Some("~tap"));
        let empty = read_spec("(~tap)").expect("empty");
        assert_eq!(s.write_spec("Tap", &empty).as_deref(), Some("~tap"));
        // A `Tap` carrying the default size still writes bare.
        let defaulted = node_datum(
            "Tap",
            vec![("size", Datum::U64(crate::Tap::DEFAULT_SIZE as u64))],
        );
        assert_eq!(s.write_spec("Tap", &defaulted).as_deref(), Some("~tap"));
        // A non-default size round-trips.
        let sized = read_spec("(~tap #:size 512)").expect("sized");
        assert_eq!(sized.get("size").and_then(Datum::as_i64), Some(512));
        assert_eq!(
            s.write_spec("Tap", &sized).as_deref(),
            Some("(~tap #:size 512)"),
        );
    }

    #[test]
    fn other_nodes_are_not_ours() {
        // A non-plyphon node falls through (so composition tries the next sugar).
        let exprs = sexpr::read("(number 5)").expect("read");
        let args = sexpr::list_args(&exprs[0]).expect("list");
        assert!(
            PlyphonSugar
                .read_spec("number", SugarArgs::new(&args[1..], "(number 5)"))
                .expect("ok")
                .is_none()
        );
        assert!(PlyphonSugar.read_bare("number").is_none());
        assert!(
            PlyphonSugar
                .write_spec("Number", &node_datum("Number", vec![]))
                .is_none()
        );
    }
}
