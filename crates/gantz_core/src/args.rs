//! The `%args` map: per-evaluation inputs the entrypoint *caller* provides to the
//! VM, readable by any node's [`expr`](crate::Node::expr) via the
//! [`ARGS`](crate::ARGS) global.
//!
//! It mirrors `%root-state` (see [`ROOT_STATE`](crate::ROOT_STATE)): a top-level
//! global the caller sets before invoking an entry fn, rather than a value threaded
//! through every function signature. The entry fn stays nullary, so existing
//! callers that don't set `%args` are unaffected - they see the [`default`].
//!
//! The one key so far is [`TIME`]: the monotonic firing time of the evaluation, in
//! seconds, which timing-sensitive nodes (e.g. DSP control inputs) read to stamp
//! their output. A node reads it from Steel as `(hash-ref %args 'time)`.

use steel::{SteelVal, gc::Gc};

/// The `%args` key holding the entrypoint firing time, in monotonic seconds.
pub const TIME: &str = "time";

/// An `%args` map carrying the firing `time` (monotonic seconds).
pub fn time(secs: f64) -> SteelVal {
    let map = steel::HashMap::new().update(SteelVal::SymbolV(TIME.into()), SteelVal::NumV(secs));
    SteelVal::HashMapV(Gc::new(map).into())
}

/// The default `%args` registered at VM init: `time` is `0.0`, so a node reading
/// `(hash-ref %args 'time)` is always valid even when no caller has set `%args`.
pub fn default() -> SteelVal {
    time(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use steel::steel_vm::engine::Engine;

    #[test]
    fn args_time_roundtrips_through_the_vm() {
        // Mirrors the runtime: register the default, then let the caller update it
        // before an entry fn would read `(hash-ref %args 'time)`.
        let mut vm = Engine::new_base();
        vm.register_value(crate::ARGS, default());
        let v = vm.run("(hash-ref %args 'time)").unwrap();
        assert_eq!(v.last(), Some(&SteelVal::NumV(0.0)));

        vm.update_value(crate::ARGS, time(1.5));
        let v = vm.run("(hash-ref %args 'time)").unwrap();
        assert_eq!(v.last(), Some(&SteelVal::NumV(1.5)));
    }
}
