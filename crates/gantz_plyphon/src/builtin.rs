//! Builtin specs for the DSP node set.

use crate::node::{Bus, Lag, Out, Pack, PlayBuf, ScopeOut, SinOsc, Sum, Unpack};
use gantz_core::{Builtin, FromNode};

/// Builtin specs for the DSP node set.
pub fn builtins<N>() -> Vec<Builtin<N>>
where
    N: FromNode<Bus>
        + FromNode<Lag>
        + FromNode<Out>
        + FromNode<Pack>
        + FromNode<PlayBuf>
        + FromNode<ScopeOut>
        + FromNode<SinOsc>
        + FromNode<Sum>
        + FromNode<Unpack>,
{
    vec![
        Builtin::new("~bus", || N::from_node(Bus::default())),
        Builtin::new("~lag", || N::from_node(Lag::default())),
        Builtin::new("~out", || N::from_node(Out::default())),
        Builtin::new("~pack", || N::from_node(Pack::default())),
        Builtin::new("~playbuf", || N::from_node(PlayBuf::default())),
        Builtin::new("~scopeout", || N::from_node(ScopeOut::default())),
        Builtin::new("~sinosc", || N::from_node(SinOsc::default())),
        Builtin::new("~sum", || N::from_node(Sum::default())),
        Builtin::new("~unpack", || N::from_node(Unpack::default())),
    ]
}
