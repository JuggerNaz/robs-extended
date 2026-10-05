//! Shell layout constants — the single source of truth.
//!
//! Slint cannot import Rust constants, so the same numbers are mirrored by
//! hand in `ui/theme.slint` (the `Theme` global's `topbar-h`, `rail-w`,
//! `right-w`, `actions-h`, `data-h`, `scenebar-h` properties). When editing
//! a value here, update the Slint mirror in the same change; the unit test
//! below pins the expected numbers so accidental edits are caught.

/// Height of the top bar (hamburger menu + annotation toolbar).
pub const TOPBAR_H: f32 = 40.0;
/// Width of the scenes rail (left panel).
pub const RAIL_W: f32 = 260.0;
/// Width of the right-hand dock (audio / chat / stats).
pub const RIGHT_W: f32 = 300.0;
/// Height of the quick-actions bar (bottom).
pub const ACTIONS_H: f32 = 56.0;
/// Bottom data-string telemetry bar height (always shown).
pub const DATA_H: f32 = 60.0;
/// Scene selector bar above the preview canvas (always shown).
pub const SCENEBAR_H: f32 = 44.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the shell layout constants. These values are hand-mirrored in
    /// `ui/theme.slint` (`Theme`: `topbar-h`, `rail-w`, `right-w`,
    /// `actions-h`, `data-h`, `scenebar-h`) — if this test fails, update the
    /// Slint mirror to match (or change both deliberately).
    #[test]
    fn shell_layout_constants_are_pinned_to_the_theme_mirror() {
        assert_eq!(TOPBAR_H, 40.0);
        assert_eq!(RAIL_W, 260.0);
        assert_eq!(RIGHT_W, 300.0);
        assert_eq!(ACTIONS_H, 56.0);
        assert_eq!(DATA_H, 60.0);
        assert_eq!(SCENEBAR_H, 44.0);
    }
}
