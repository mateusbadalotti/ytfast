//! ytfast's internals, exposed so the binary can reach them.

pub mod app;
pub mod audio;
pub mod auth;
pub mod backend;
#[cfg(target_os = "macos")]
pub mod helium;
pub mod images;
pub mod innertube;
pub mod lyrics;
#[cfg(target_os = "macos")]
pub mod mac_menu;
#[cfg(target_os = "macos")]
pub mod mac_output;
pub mod model;
pub mod player;
pub mod queue;
pub mod settings;
pub mod stream;
pub mod theme;
pub mod ui;
#[cfg(target_os = "macos")]
pub mod update;
