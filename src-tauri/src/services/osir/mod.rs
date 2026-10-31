//! OSIR desktop account integration boundary.
//!
//! Browser login requires native platform support and server protocol readiness.
//! Never treat the one-time Codex configuration exchange as account login.

pub mod account;
pub mod auth;
pub mod authorization;
pub mod client;
pub mod codex_desktop;
#[path = "desktop_install/transaction.rs"]
pub(crate) mod desktop_install;
// Offline fixture coverage only; no production history repair API is exposed.
pub mod credentials;
#[cfg(test)]
mod history;
pub mod history_inspect;
pub mod history_repair;
pub mod key_vault;
pub mod managed;
pub mod models;
pub mod sync;
pub mod usage;
pub mod windows_codex_package;

pub mod session;

pub mod connection;
pub mod gateway;
pub mod resources;
pub mod workbuddy;

pub mod request_budget;

pub(crate) mod image_binding;

pub mod legacy_connect;

pub mod legacy_snapshot;
