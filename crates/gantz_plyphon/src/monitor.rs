//! Writing sampled dsp values back into a monitor node's ring-buffer state.
//!
//! A monitor node (`~scopeout`) holds its recent samples as a plain Steel list
//! ([`SteelVal::ListV`]) in VM state - the same representation `plot` uses for a
//! scope history. Each frame the audio driver drains its `ScopeOut` scope stream and
//! calls [`push_ring`] to append the samples, capping the ring at the node's
//! configured length. The node's control `expr` surfaces this state on a trigger push.

use gantz_core::node::state;
use gantz_core::steel::SteelVal;
use gantz_core::steel::steel_vm::engine::Engine;

/// Append `values` to the ring-buffer list at the node `path` in VM state, dropping
/// the oldest so the ring holds at most `size` samples (a `size` of 0 is treated as
/// 1 - the ring always keeps at least the latest sample).
///
/// The state is a [`SteelVal::ListV`] of numbers (seeded empty in the node's
/// `register`); a non-list or absent value is treated as an empty ring.
///
/// The ring is rebuilt in a single `collect` rather than element-by-element: steel's
/// list is an unrolled persistent list whose `push_back` is O(n), so appending a whole
/// frame's samples (hundreds, at the full audio rate) one at a time was O(frame x ring)
/// on the main thread every frame. When the frame alone fills the ring, the prior
/// state is dropped without being read.
pub fn push_ring(vm: &mut Engine, path: &[usize], values: &[f32], size: usize) {
    let size = size.max(1);
    let sample = |&v: &f32| SteelVal::NumV(v as f64);

    // Fast path: this frame alone fills (or overfills) the ring - keep its last `size`
    // samples and drop the prior ring without reading it.
    if values.len() >= size {
        let tail = values[values.len() - size..].iter().map(sample).collect();
        let _ = state::update_value(vm, path, SteelVal::ListV(tail));
        return;
    }

    // Otherwise keep the tail of the old ring so it plus the frame totals `size`.
    let old = match state::extract_value(vm, path) {
        Ok(Some(SteelVal::ListV(list))) => list,
        _ => Default::default(),
    };
    let keep = size - values.len();
    let skip = old.len().saturating_sub(keep);
    let ring = old
        .iter()
        .cloned()
        .skip(skip)
        .chain(values.iter().map(sample))
        .collect();
    let _ = state::update_value(vm, path, SteelVal::ListV(ring));
}

#[cfg(test)]
mod tests {
    use super::*;
    use gantz_core::steel::steel_vm::engine::Engine;

    /// Seed an empty ring at `[0]`, then push samples in batches: the ring keeps
    /// only the most recent `size`, oldest-dropped-first.
    #[test]
    fn push_ring_caps_and_drops_oldest() {
        let mut vm = Engine::new_base();
        vm.register_value(gantz_core::ROOT_STATE, SteelVal::empty_hashmap());
        state::init_value_if_absent(&mut vm, &[0], || SteelVal::ListV(Default::default())).unwrap();

        // Fill past capacity in two batches; only the last `size` survive.
        push_ring(&mut vm, &[0], &[1.0, 2.0, 3.0], 4);
        push_ring(&mut vm, &[0], &[4.0, 5.0], 4);

        let got = ring_values(&mut vm, &[0]);
        assert_eq!(
            got,
            vec![2.0, 3.0, 4.0, 5.0],
            "ring keeps the newest `size`"
        );
    }

    /// A frame at least as long as `size` replaces the ring with its own last `size`
    /// samples (the fast path drops the prior ring rather than appending to it).
    #[test]
    fn push_ring_full_frame_replaces() {
        let mut vm = Engine::new_base();
        vm.register_value(gantz_core::ROOT_STATE, SteelVal::empty_hashmap());
        state::init_value_if_absent(&mut vm, &[0], || SteelVal::ListV(Default::default())).unwrap();
        push_ring(&mut vm, &[0], &[1.0, 2.0], 2);
        push_ring(&mut vm, &[0], &[3.0, 4.0, 5.0], 2);
        assert_eq!(ring_values(&mut vm, &[0]), vec![4.0, 5.0]);
    }

    /// A `size` of 0 is clamped to 1 - the ring keeps the single latest sample.
    #[test]
    fn push_ring_size_zero_keeps_latest() {
        let mut vm = Engine::new_base();
        vm.register_value(gantz_core::ROOT_STATE, SteelVal::empty_hashmap());
        state::init_value_if_absent(&mut vm, &[0], || SteelVal::ListV(Default::default())).unwrap();
        push_ring(&mut vm, &[0], &[1.0, 2.0, 3.0], 0);
        assert_eq!(ring_values(&mut vm, &[0]), vec![3.0]);
    }

    /// Read the ring at `path` back as `f64`s.
    fn ring_values(vm: &mut Engine, path: &[usize]) -> Vec<f64> {
        match state::extract_value(vm, path) {
            Ok(Some(SteelVal::ListV(list))) => list
                .iter()
                .filter_map(|v| match v {
                    SteelVal::NumV(f) => Some(*f),
                    SteelVal::IntV(i) => Some(*i as f64),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}
