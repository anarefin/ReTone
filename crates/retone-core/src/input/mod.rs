//! The macOS input layer's logic (phase 3): selection reads, synthetic copy,
//! paste with clipboard save/restore, target capture and activation, and the
//! Accessibility check. OS calls go through the traits in [`ports`];
//! `retone-macos` implements them and the app crate wires them up.

#[cfg(test)]
pub(crate) mod fakes;
pub mod layer;
pub mod layout;
pub mod ports;
pub mod signatures;

pub use layer::{
    InputLayer, InputPorts, PasteError, PasteOptions, PasteOutcome, ReadOptions, SelectAllError,
    SelectionRead, TargetApp, UnavailableCode,
};
