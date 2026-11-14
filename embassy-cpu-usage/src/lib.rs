//! Per-core and per-task CPU usage measurement for [Embassy] on RP2040 / RP2350.
//!
//! This crate measures how much wall-clock time each core spends executing
//! tasks, and breaks that down per task. It works by hooking the
//! `embassy-executor` tracing callbacks that fire around every task poll, so it
//! needs no changes to your executor setup and no forked executor.
//!
//! # How it works
//!
//! CPU usage is reported as `busy / window`, where `busy` is the union of task
//! execution intervals on a core and `window` is the wall-clock time since the
//! last reset. This model counts `InterruptExecutor` task time correctly and
//! handles preemption nesting (a thread task interrupted by a higher-priority
//! interrupt-executor task on the same core).
//!
//! It does **not** count time spent in raw hardware ISRs (DMA completion, your
//! own `#[interrupt]` handlers) because those are not tasks. If you do heavy
//! work in ISRs, the reported figure will be lower than true core load. Pair
//! this with a SysTick PC-sampling profiler if you need to capture that.
//!
//! # Usage
//!
//! 1. Enable the `trace` feature on your `embassy-executor` dependency (do
//!    **not** also enable `rtos-trace`; that would define the same hook symbols
//!    and cause duplicate-symbol link errors).
//! 2. Enable `critical-section-impl` on `embassy-rp` (the accounting tables are
//!    shared across cores and rely on a multicore-safe critical section).
//! 3. Call [`init`] once at startup, then call [`report`] (or [`log`]) from a
//!    low-frequency task.
//!
//! ```ignore
//! #[embassy_executor::task]
//! async fn cpu_report() {
//!     embassy_cpu_usage::init();
//!     loop {
//!         embassy_time::Timer::after_secs(1).await;
//!         embassy_cpu_usage::log(true); // print via defmt and reset the window
//!     }
//! }
//! ```
//!
//! # If the built-in hooks don't link
//!
//! The `trace-hooks` feature (on by default) defines the `_embassy_trace_*`
//! symbols the executor calls. Their exact set and signatures can vary between
//! `embassy-executor` versions. If you get an "undefined symbol" link error for
//! another `_embassy_trace_*` name, or you suspect a signature mismatch,
//! disable the `trace-hooks` feature and call [`task_begin`] / [`task_end`]
//! yourself from correctly-typed hooks (or from an `rtos_trace::RtosTrace`
//! implementation). The accounting engine is fully public.
//!
//! [Embassy]: https://embassy.dev

#![no_std]
#![allow(clippy::needless_range_loop)]

use core::cell::RefCell;
use critical_section::Mutex;
use embassy_time::Instant;

/// Number of cores tracked. Defaults to 2 (RP2040 / RP2350). Enable the
/// `single-core` feature to set it to 1.
#[cfg(feature = "single-core")]
pub const NCORES: usize = 1;
#[cfg(not(feature = "single-core"))]
pub const NCORES: usize = 2;

/// Maximum number of distinct tasks tracked. Defaults to 64; enable `large`
/// for 128. Tasks beyond this limit are merged into the last slot.
#[cfg(feature = "large")]
pub const MAX_TASKS: usize = 128;
#[cfg(not(feature = "large"))]
pub const MAX_TASKS: usize = 64;

/// Maximum preemption nesting depth per core (thread task plus
/// interrupt-executor priority levels).
pub const MAX_DEPTH: usize = 8;

// SIO base is 0xd000_0000 on both RP2040 and RP2350; the CPUID register sits at
// offset 0 and reads back the current core index (0 or 1).
const SIO_CPUID: *const u32 = 0xd000_0000 as *const u32;

#[inline]
fn core_id() -> usize {
    if NCORES == 1 {
        return 0;
    }
    let id = unsafe { core::ptr::read_volatile(SIO_CPUID) } as usize;
    if id < NCORES {
        id
    } else {
        NCORES - 1
    }
}

#[derive(Clone, Copy)]
struct TaskAcct {
    id: u32,
    core: u8,
    ticks: u64,
}

#[derive(Clone, Copy)]
struct CoreState {
    stack: [u32; MAX_DEPTH], // executing task ids; top of stack = running now
    depth: usize,
    last_ts: u64, // timer ticks at the last transition
    busy: u64,    // union of busy ticks accumulated this window
}
impl CoreState {
    const NEW: Self = Self {
        stack: [0; MAX_DEPTH],
        depth: 0,
        last_ts: 0,
        busy: 0,
    };
}

struct Monitor {
    cores: [CoreState; NCORES],
    tasks: [TaskAcct; MAX_TASKS],
    ntasks: usize,
    window_start: u64,
}
impl Monitor {
    const NEW: Self = Self {
        cores: [CoreState::NEW; NCORES],
        tasks: [TaskAcct {
            id: 0,
            core: 0,
            ticks: 0,
        }; MAX_TASKS],
        ntasks: 0,
        window_start: 0,
    };
}

static MON: Mutex<RefCell<Monitor>> = Mutex::new(RefCell::new(Monitor::NEW));

fn task_slot(m: &mut Monitor, id: u32) -> usize {
    for i in 0..m.ntasks {
        if m.tasks[i].id == id {
            return i;
        }
    }
    if m.ntasks < MAX_TASKS {
        let i = m.ntasks;
        m.tasks[i] = TaskAcct {
            id,
            core: 0,
            ticks: 0,
        };
        m.ntasks += 1;
        i
    } else {
        MAX_TASKS - 1 // overflow bucket
    }
}

fn transition(now: u64, core: usize, push: Option<u32>, pop: bool) {
    critical_section::with(|cs| {
        let mut guard = MON.borrow(cs).borrow_mut();
        let m = &mut *guard;

        if m.window_start == 0 {
            m.window_start = now;
        }

        // Charge time since the last transition to whoever was running.
        let depth = m.cores[core].depth;
        if depth > 0 {
            let dt = now.wrapping_sub(m.cores[core].last_ts);
            let top = m.cores[core].stack[depth - 1];
            m.cores[core].busy = m.cores[core].busy.wrapping_add(dt);
            let slot = task_slot(m, top);
            m.tasks[slot].ticks = m.tasks[slot].ticks.wrapping_add(dt);
            m.tasks[slot].core = core as u8;
        }

        // Apply the stack change.
        let c = &mut m.cores[core];
        if pop && c.depth > 0 {
            c.depth -= 1;
        }
        if let Some(id) = push {
            if c.depth < MAX_DEPTH {
                c.stack[c.depth] = id;
                c.depth += 1;
            }
        }
        c.last_ts = now;
    });
}

/// Initialize measurement and start the timing window now.
///
/// Optional: the window also starts lazily on the first tracked event. Calling
/// this at startup gives a clean first reporting interval and guarantees the
/// crate (and therefore its trace hooks) is linked in.
pub fn init() {
    let now = Instant::now().as_ticks();
    critical_section::with(|cs| {
        MON.borrow(cs).borrow_mut().window_start = now;
    });
}

/// Record that a task began executing on the current core.
///
/// Called for you by the built-in trace hooks when the `trace-hooks` feature is
/// enabled. Call it yourself only if you disabled that feature and are wiring
/// the executor trace callbacks manually. `task_id` should be a stable
/// per-task identifier; the executor passes the task's static address.
#[inline]
pub fn task_begin(task_id: u32) {
    transition(Instant::now().as_ticks(), core_id(), Some(task_id), false);
}

/// Record that the current task on the current core finished executing.
///
/// See [`task_begin`].
#[inline]
pub fn task_end() {
    transition(Instant::now().as_ticks(), core_id(), None, true);
}

/// Visit current usage.
///
/// `per_core` is called once per core with `(core_index, permille)` where
/// permille is 0..=1000 (tenths of a percent). `per_task` is called once per
/// tracked task with `(core_index, task_id, permille)`.
///
/// Closures are invoked *after* the internal critical section is released, so
/// it is safe to do slow work (e.g. `defmt` over RTT) inside them without
/// stalling the other core.
///
/// If `reset_after` is true, all counters and the window are reset afterward,
/// so each call reports usage since the previous call.
pub fn report(
    reset_after: bool,
    mut per_core: impl FnMut(u8, u16),
    mut per_task: impl FnMut(u8, u32, u16),
) {
    let now = Instant::now().as_ticks();

    let mut cores = [0u16; NCORES];
    let mut tasks: [(u8, u32, u16); MAX_TASKS] = [(0, 0, 0); MAX_TASKS];
    let mut ntasks = 0usize;

    critical_section::with(|cs| {
        let mut guard = MON.borrow(cs).borrow_mut();
        let m = &mut *guard;

        let window = now.wrapping_sub(m.window_start).max(1);

        for ci in 0..NCORES {
            cores[ci] = (m.cores[ci].busy.saturating_mul(1000) / window) as u16;
        }
        ntasks = m.ntasks;
        for i in 0..m.ntasks {
            let permille = (m.tasks[i].ticks.saturating_mul(1000) / window) as u16;
            tasks[i] = (m.tasks[i].core, m.tasks[i].id, permille);
        }

        if reset_after {
            for ci in 0..NCORES {
                m.cores[ci].busy = 0;
            }
            for i in 0..m.ntasks {
                m.tasks[i].ticks = 0;
            }
            m.window_start = now;
        }
    });

    for ci in 0..NCORES {
        per_core(ci as u8, cores[ci]);
    }
    for i in 0..ntasks {
        let (core, id, permille) = tasks[i];
        per_task(core, id, permille);
    }
}

/// Reset all counters and restart the timing window.
pub fn reset() {
    let now = Instant::now().as_ticks();
    critical_section::with(|cs| {
        let mut guard = MON.borrow(cs).borrow_mut();
        let m = &mut *guard;
        for ci in 0..NCORES {
            m.cores[ci].busy = 0;
        }
        for i in 0..m.ntasks {
            m.tasks[i].ticks = 0;
        }
        m.window_start = now;
    });
}

/// Print a usage report via `defmt` at INFO level, optionally resetting the
/// window. Task ids are the tasks' static addresses; resolve them to names with
/// `cargo nm` / your linker map, or keep your own id-to-name table.
#[cfg(feature = "defmt")]
pub fn log(reset_after: bool) {
    defmt::info!("--- CPU usage ---");
    report(
        reset_after,
        |core, p| defmt::info!("core{=u8}: {=u16}.{=u16}% busy", core, p / 10, p % 10),
        |core, id, p| {
            defmt::info!(
                "  [core{=u8}] task {=u32:#x}: {=u16}.{=u16}%",
                core,
                id,
                p / 10,
                p % 10
            )
        },
    );
}

// -------------------------------------------------------------------------
// Built-in embassy-executor trace hooks.
//
// These satisfy the `extern "Rust"` symbols the executor references when its
// `trace` feature is enabled. If your embassy-executor version references a
// different set or different signatures, disable this crate's `trace-hooks`
// feature and call `task_begin` / `task_end` from your own hooks instead.
// -------------------------------------------------------------------------
#[cfg(feature = "trace-hooks")]
mod hooks {
    #[no_mangle]
    extern "Rust" fn _embassy_trace_task_exec_begin(_executor_id: u32, task_id: u32) {
        super::task_begin(task_id);
    }

    #[no_mangle]
    extern "Rust" fn _embassy_trace_task_end(_executor_id: u32, _task_id: u32) {
        super::task_end();
    }

    #[no_mangle]
    extern "Rust" fn _embassy_trace_task_exec_end(_executor_id: u32, _task_id: u32) {
        super::task_end();
    }

    // The remaining hooks are not needed for CPU accounting, but must exist so
    // the executor links. They are intentionally empty.
    #[no_mangle]
    extern "Rust" fn _embassy_trace_task_new(_executor_id: u32, _task_id: u32) {}

    #[no_mangle]
    extern "Rust" fn _embassy_trace_task_ready_begin(_executor_id: u32, _task_id: u32) {}

    #[no_mangle]
    extern "Rust" fn _embassy_trace_executor_idle(_executor_id: u32) {}

    #[no_mangle]
    extern "Rust" fn _embassy_trace_poll_start(_executor_id: u32) {}
}
