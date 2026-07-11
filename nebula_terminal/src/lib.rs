//! Nebula - The GPU Enhanced Terminal.

#![warn(rust_2018_idioms, future_incompatible)]
#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use)]
#![cfg_attr(clippy, deny(warnings))]

pub mod event;
pub mod event_loop;
pub mod grid;
pub mod index;
pub mod osc_cwd;
pub mod selection;
pub mod sync;
pub mod term;
pub mod thread;
pub mod tty;
pub mod vi_mode;

pub use crate::grid::Grid;
pub use crate::term::Term;

/// PTY-side startup profiling (`NEBULA_BOOT_TRACE=1`): times the ConPTY
/// bring-up stages the app-side boot trace cannot see — they happen inside
/// this crate and the console host (CreatePseudoConsole, shell attach, the
/// host's DA1 handshake window until the first conout bytes arrive).
pub(crate) fn pty_trace(label: &str) {
    use std::sync::OnceLock;
    use std::time::Instant;
    static T0: OnceLock<Instant> = OnceLock::new();
    static ON: OnceLock<bool> = OnceLock::new();
    let t0 = *T0.get_or_init(Instant::now);
    if *ON.get_or_init(|| std::env::var_os("NEBULA_BOOT_TRACE").is_some()) {
        eprintln!("[pty  +{:>7.1}ms] {label}", t0.elapsed().as_secs_f64() * 1000.0);
    }
}
pub use vte;
