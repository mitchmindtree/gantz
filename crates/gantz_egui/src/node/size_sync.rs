//! Committed-size synchronisation for resizable node bodies (comment, plot).
//!
//! A node's committed size is part of its content address, while the
//! *displayed* size lives in egui's persisted `Resize` state. Left
//! unreconciled, the two feed back: external changes (undo, collab sync,
//! merge) neither display nor survive - the next frame overwrites them from
//! the local layout and mints a spurious commit - and collaborating peers
//! whose rendered sizes round differently "correct" each other in an endless
//! commit ping-pong.
//!
//! [`size_sync_frame`] is the per-frame state machine both nodes share:
//! external changes *push* into the display (a one-frame
//! `Resize::fixed_size` container - egui clamps persisted state through
//! min/max and stores it back, overriding stale state) and never commit; the
//! committed size is written only by genuine local interaction (a settled
//! resize-corner release, or a node-specific edit such as a comment text
//! flush).

/// Per-frame size bookkeeping stored in egui temp memory, keyed by the
/// node's egui id `.with("size_sync")`.
///
/// `last_seen` is the node's committed size at the end of the last UI pass:
/// a mismatch at frame start means the value was replaced *externally*
/// (undo, collab sync, merge), which must update the display rather than be
/// clobbered by it. Keyed by node index, so index reuse after a deletion can
/// leave a stale entry - the mismatch then reads as an external change,
/// which self-heals (push, no commit).
#[derive(Clone, Copy)]
pub(crate) struct SizeSync {
    pub last_seen: [u16; 2],
    pub was_resizing: bool,
}

/// The size-sync decisions for one frame: `(push_external, drag_released)`.
///
/// - `push_external`: the committed size must be pushed into the displayed
///   resize state (first frame under this id, or the node value changed
///   externally). An external change also wins over an in-flight drag.
/// - `drag_released`: the resize corner was released since the last frame -
///   the interaction allowed to write the committed size.
pub(crate) fn size_sync_frame(
    prev: Option<SizeSync>,
    size: [u16; 2],
    resizing: bool,
) -> (bool, bool) {
    match prev {
        None => (true, false),
        Some(prev) => {
            let push = prev.last_seen != size;
            let released = !push && prev.was_resizing && !resizing;
            (push, released)
        }
    }
}

/// The committed form of a rendered size: rounded rather than truncated, so
/// two machines whose rendered sizes straddle an integer don't disagree by a
/// whole unit.
pub(crate) fn fitted_size(width: f32, height: f32) -> [u16; 2] {
    [width.round() as u16, height.round() as u16]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The size-sync state machine: external changes always push into the
    /// display (never commit); a commit-worthy release fires only on a
    /// clean corner-drag `true -> false` transition.
    #[test]
    fn size_sync_frame_decisions() {
        let sync = |last_seen, was_resizing| {
            Some(SizeSync {
                last_seen,
                was_resizing,
            })
        };
        // First frame under this id: push, never commit.
        assert_eq!(size_sync_frame(None, [100, 40], false), (true, false));
        // Steady state: nothing to do.
        assert_eq!(
            size_sync_frame(sync([100, 40], false), [100, 40], false),
            (false, false)
        );
        // External change: push - even mid-drag (external wins, no release).
        assert_eq!(
            size_sync_frame(sync([100, 40], true), [120, 40], true),
            (true, false)
        );
        // Drag in progress (starting or continuing): no push, no release.
        assert_eq!(
            size_sync_frame(sync([100, 40], false), [100, 40], true),
            (false, false)
        );
        assert_eq!(
            size_sync_frame(sync([100, 40], true), [100, 40], true),
            (false, false)
        );
        // Release transition: commit-worthy.
        assert_eq!(
            size_sync_frame(sync([100, 40], true), [100, 40], false),
            (false, true)
        );
    }

    #[test]
    fn fitted_size_rounds() {
        assert_eq!(fitted_size(100.6, 40.4), [101, 40]);
        assert_eq!(fitted_size(100.4, 40.5), [100, 41]);
    }
}
