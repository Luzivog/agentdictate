//! Linux adapters for AgentDictate's runtime seams.

pub mod audio_ducking;
pub mod clipboard;
pub mod command;
pub mod focus;
pub mod hotkey;
pub mod injection;
#[cfg(feature = "native-hotkey")]
pub mod native_hotkey;
pub mod overlay_placement;
pub mod paste;
pub mod recorder;
