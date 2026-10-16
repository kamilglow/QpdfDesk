//! Dev builds only (`QD_LAG`): how long the UI thread keeps a callback waiting
//! (the window must never stall for more than 50 ms). A thread asks every
//! 5 ms for a callback on the UI thread and the callback notes how late it ran — FLTK runs it at the next
//! turn of the event loop, so the delay is the time the thread was busy.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const EVERY: Duration = Duration::from_millis(5);
/// The longest the window may stall, in milliseconds.
const LIMIT_MS: u64 = 50;

static MAX_US: AtomicU64 = AtomicU64::new(0);
static SAMPLES: AtomicU64 = AtomicU64::new(0);
/// `(ms since the start, ms late)` for every wait above [`LIMIT_MS`].
static OVER: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

/// Start measuring; returns when it started.
pub(super) fn start() -> Instant {
    let started = Instant::now();
    std::thread::spawn(move || loop {
        std::thread::sleep(EVERY);
        let sent = Instant::now();
        fltk::app::awake_callback(move || {
            let late = sent.elapsed().as_micros() as u64;
            SAMPLES.fetch_add(1, Ordering::Relaxed);
            MAX_US.fetch_max(late, Ordering::Relaxed);
            if late > LIMIT_MS * 1000 {
                if let Ok(mut over) = OVER.lock() {
                    over.push((sent.duration_since(started).as_millis() as u64, late / 1000));
                }
            }
        });
    });
    started
}

/// What was measured, as text.
pub(super) fn report(started: Instant) -> String {
    let over = OVER.lock().map(|over| over.clone()).unwrap_or_default();
    let mut text = format!(
        "samples {} in {} ms, longest wait {:.1} ms, over {LIMIT_MS} ms: {}\r\n",
        SAMPLES.load(Ordering::Relaxed),
        started.elapsed().as_millis(),
        MAX_US.load(Ordering::Relaxed) as f64 / 1000.0,
        over.len()
    );
    for (at, late) in over {
        text.push_str(&format!("  at {at} ms: {late} ms\r\n"));
    }
    text
}
