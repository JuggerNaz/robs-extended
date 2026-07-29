//! UI panels split out of `app.rs`.
//!
//! Each submodule owns one top-level panel/window that `update()` lays out:
//! the menu bar, the bottom streaming-controls strip, the left scenes/sources
//! panel, the source-properties modal, the central preview, and the right-hand
//! mixer / chat / stats / event-log panel.

pub(crate) mod bottom;
pub(crate) mod menu;
pub(crate) mod preview;
pub(crate) mod right;
pub(crate) mod scenes;
pub(crate) mod sources;
