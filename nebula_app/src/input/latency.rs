//! Input latency probes.
//!
//! Segment timestamps for the input pipeline, logged through the same
//! `nebula_debug_log` channel used by ad-hoc perf investigations. Off by
//! default; set `NEBULA_INPUT_LATENCY=1` before launch to enable (checked
//! lazily on first probe). When disabled every probe is one
//! initialized-`OnceLock` read.
//!
//! Segments (all measured on the event-loop thread):
//!
//! - `key→pty`: key event entered `key_input` → bytes handed to the PTY
//!   writer channel. Pure encode + dispatch cost.
//! - `wake→frame`: first PTY wakeup since the last presented frame → end of
//!   `draw`. Scheduling plus render cost for terminal output.
//! - `key→frame`: key event → end of the next `draw`. This is the local
//!   response bound (cursor/IME/selection feedback), NOT echo latency: the
//!   echo may land frames later after a full PTY round-trip. When a wakeup
//!   arrived between the key and the frame, `key→frame` approximates the
//!   echo path for fast shells.
//!
//! Known deviations, accepted for a diagnostic skeleton: one global slot set
//! (no per-window keying — concurrent typing into several windows interleaves
//! measurements), and no echo-byte attribution (would require OSC 133 /
//! prompt-mark correlation).

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static ENABLED: OnceLock<bool> = OnceLock::new();

#[derive(Default)]
struct Slots {
    key: Option<Instant>,
    wakeup: Option<Instant>,
}

static SLOTS: Mutex<Slots> = Mutex::new(Slots { key: None, wakeup: None });

#[inline]
fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        let on = std::env::var("NEBULA_INPUT_LATENCY").is_ok_and(|v| v != "0" && !v.is_empty());
        if on {
            crate::display::nebula_debug_log(
                "inlat enabled (key→pty / wake→frame / key→frame)",
            );
        }
        on
    })
}

/// A key press entered the input processor.
#[inline]
pub fn key_received() {
    if !enabled() {
        return;
    }
    SLOTS.lock().unwrap().key = Some(Instant::now());
}

/// The key's bytes were handed to the PTY writer.
#[inline]
pub fn key_written_to_pty() {
    if !enabled() {
        return;
    }
    let key = SLOTS.lock().unwrap().key;
    if let Some(key) = key {
        log_segment("key→pty", key);
    }
}

/// PTY output woke the event loop; keep only the earliest wakeup per frame.
#[inline]
pub fn pty_wakeup() {
    if !enabled() {
        return;
    }
    let slots = &mut *SLOTS.lock().unwrap();
    if slots.wakeup.is_none() {
        slots.wakeup = Some(Instant::now());
    }
}

/// A frame finished drawing; close and report any open segments.
#[inline]
pub fn frame_drawn() {
    if !enabled() {
        return;
    }
    let taken = {
        let slots = &mut *SLOTS.lock().unwrap();
        (slots.key.take(), slots.wakeup.take())
    };
    if let Some(wakeup) = taken.1 {
        log_segment("wake→frame", wakeup);
    }
    if let Some(key) = taken.0 {
        log_segment("key→frame", key);
    }
}

fn log_segment(name: &str, since: Instant) {
    let micros = since.elapsed().as_micros();
    crate::display::nebula_debug_log(format!(
        "inlat {name} {}.{:03}ms",
        micros / 1000,
        micros % 1000
    ));
}
