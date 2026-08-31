//! The embassy-executor trace hooks, defined by hand.
//!
//! The crate's `trace-hooks` feature is a *macro* feature: it emits these
//! symbols at the graph declaration site, and the playground builds its graph
//! at runtime — so the seven hooks live here instead, forwarding the five the
//! supervisor's recorders consume (`supervisor-macros` emits the same set).
//!
//! Durations need a second clock. `trace::now_ticks` reads the mock driver,
//! which only advances between polls (the page's rAF loop), never during one —
//! so every `exec_ticks`/`max_poll_ticks` reading is exactly zero and all
//! elapsed mock time lands in `idle_ticks`. Counts are genuine; for durations
//! these hooks also stamp a wall clock (`performance.now()` in the browser)
//! into per-node and per-executor tables, reported as *browser time*.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::registry;

/// Wall-clock microseconds (browser `performance.now()`; std `Instant`).
#[cfg(target_arch = "wasm32")]
pub fn wall_us() -> u64 {
    #[wasm_bindgen::prelude::wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = performance)]
        fn now() -> f64;
    }
    (now() * 1000.0) as u64
}

#[cfg(not(target_arch = "wasm32"))]
pub fn wall_us() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// Per-executor wall-time ledger, keyed like `trace`'s own slot table.
pub struct ExecWall {
    pub id: AtomicU32,
    begin_us: AtomicU64,
    pub exec_us: AtomicU64,
}

#[allow(clippy::declare_interior_mutable_const)]
const FREE: ExecWall = ExecWall {
    id: AtomicU32::new(0),
    begin_us: AtomicU64::new(0),
    exec_us: AtomicU64::new(0),
};

pub static EXEC_WALL: [ExecWall; 8] = [FREE; 8];

fn wall_slot(executor_id: u32) -> Option<&'static ExecWall> {
    for s in &EXEC_WALL {
        if s.id.load(Ordering::Relaxed) == executor_id {
            return Some(s);
        }
    }
    EXEC_WALL.iter().find(|s| {
        s.id.compare_exchange(0, executor_id, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    })
}

#[unsafe(no_mangle)]
fn _embassy_trace_poll_start(executor_id: u32) {
    embassy_supervisor::trace::on_poll_start(executor_id);
}

#[unsafe(no_mangle)]
fn _embassy_trace_task_new(_executor_id: u32, _task_id: u32) {}

#[unsafe(no_mangle)]
fn _embassy_trace_task_end(executor_id: u32, task_id: u32) {
    embassy_supervisor::trace::on_task_end(executor_id, task_id);
}

#[unsafe(no_mangle)]
fn _embassy_trace_task_exec_begin(executor_id: u32, task_id: u32) {
    embassy_supervisor::trace::on_task_exec_begin(executor_id, task_id);
    if let Some(s) = wall_slot(executor_id) {
        s.begin_us.store(wall_us(), Ordering::Relaxed);
    }
}

#[unsafe(no_mangle)]
fn _embassy_trace_task_exec_end(executor_id: u32, task_id: u32) {
    embassy_supervisor::trace::on_task_exec_end(executor_id, task_id);
    let Some(s) = wall_slot(executor_id) else {
        return;
    };
    let elapsed = wall_us().saturating_sub(s.begin_us.load(Ordering::Relaxed));
    s.exec_us.fetch_add(elapsed, Ordering::Relaxed);
    // Resolve the task id back to its node (48 max: a short scan) and book
    // the poll into its wall-time row.
    for rt in registry::nodes() {
        if rt.node.task_id() == task_id {
            rt.last_poll_us.store(elapsed as u32, Ordering::Relaxed);
            rt.max_poll_us.fetch_max(elapsed as u32, Ordering::Relaxed);
            rt.exec_us.fetch_add(elapsed, Ordering::Relaxed);
            rt.exec_id.store(executor_id, Ordering::Relaxed);
            break;
        }
    }
}

#[unsafe(no_mangle)]
fn _embassy_trace_task_ready_begin(_executor_id: u32, _task_id: u32) {}

#[unsafe(no_mangle)]
fn _embassy_trace_executor_idle(executor_id: u32) {
    embassy_supervisor::trace::on_executor_idle(executor_id);
}
