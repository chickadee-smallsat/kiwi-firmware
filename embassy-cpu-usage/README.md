# embassy-cpu-usage

Per-core and per-task CPU usage measurement for [Embassy](https://embassy.dev)
on RP2040 / RP2350.

It hooks `embassy-executor`'s tracing callbacks (fired around every task poll),
so it needs no forked executor and no changes to how you spawn tasks. Usage is
reported as `busy / window`, where `busy` is the union of task-execution
intervals on a core and `window` is wall-clock time since the last reset.

## What it measures

- **Per core:** fraction of wall-clock time the core spent executing tasks.
- **Per task:** the same, broken down by task, including interrupt-executor
  tasks and correctly accounting for preemption nesting.

## What it does NOT measure

Time spent in **raw hardware ISRs** (DMA completion, your own `#[interrupt]`
handlers) is not counted, because those are not tasks. If you do heavy work in
ISRs, the reported number will be lower than true core load. Pair this with a
SysTick PC-sampling profiler if you need to capture that.

## Setup

Add the dependency, and enable the `trace` feature on `embassy-executor`:

```toml
[dependencies]
embassy-cpu-usage = { path = "../embassy-cpu-usage" }  # or a git/version source

embassy-executor = { version = "0.10", features = [
    "platform-cortex-m",
    "executor-thread",
    "executor-interrupt",
    "defmt",
    "trace",          # <-- add this
] }
```

Requirements:

- Do **not** also enable `rtos-trace` on `embassy-executor`; it defines the same
  hook symbols and you'll get duplicate-symbol link errors.
- Keep `critical-section-impl` enabled on `embassy-rp` (you already have it).
  The accounting tables are shared across cores and rely on a multicore-safe
  critical section.
- Align the `embassy-time` and `defmt` versions in this crate's `Cargo.toml`
  with the ones in your project. Cargo unifies to one version per crate, so a
  mismatched major/minor will fail to build.

Then call `init()` once and report from a low-frequency task:

```rust
#[embassy_executor::task]
async fn cpu_report() {
    embassy_cpu_usage::init();
    loop {
        embassy_time::Timer::after_secs(1).await;
        embassy_cpu_usage::log(true); // print via defmt, reset the window
    }
}
```

Example output:

```
--- CPU usage ---
core0: 37.4% busy
core1: 12.1% busy
  [core0] task 0x2000a1c0: 31.2%
  [core0] task 0x2000a3e0: 6.2%
  [core1] task 0x2000a600: 12.1%
```

Task ids are the tasks' static addresses. Resolve them to names with
`cargo nm target/.../your-firmware | grep <addr>`, your linker map, or by
keeping your own id-to-name table.

## API

- `init()` — start the timing window (also happens lazily on first event).
- `report(reset_after, per_core, per_task)` — visit usage with your own
  closures; both receive permille (0..=1000, i.e. tenths of a percent).
  Closures run **after** the internal critical section is released, so slow
  work inside them (RTT, etc.) won't stall the other core.
- `log(reset_after)` — convenience report via `defmt` (needs the `defmt`
  feature).
- `reset()` — zero the counters and restart the window.
- `task_begin(task_id)` / `task_end()` — the accounting engine, exposed for the
  escape hatch below.

## Cargo features

| Feature       | Default | Effect                                                            |
| ------------- | ------- | ----------------------------------------------------------------- |
| `trace-hooks` | on      | Defines the `_embassy_trace_*` hook symbols the executor calls.   |
| `defmt`       | on      | Enables `log()` and `defmt` output.                               |
| `single-core` | off     | Track 1 core instead of 2.                                        |
| `large`       | off     | Track up to 128 tasks instead of 64.                              |

## If the built-in hooks don't link

The exact set and signatures of the `_embassy_trace_*` callbacks can vary
between `embassy-executor` versions. The `trace-hooks` feature defines the set
expected around 0.10. If you hit an **undefined symbol** link error for some
other `_embassy_trace_*` name, or you suspect a signature mismatch:

1. Open your vendored source at
   `~/.cargo/registry/src/*/embassy-executor-<ver>/src/raw/trace.rs` and find
   the `extern "Rust"` block — that is the authoritative list.
2. Either add the missing no-op hook, or disable this crate's `trace-hooks`
   feature and call `task_begin` / `task_end` from your own correctly-typed
   hooks (or from an `rtos_trace::RtosTrace` implementation).

```toml
embassy-cpu-usage = { path = "...", default-features = false, features = ["defmt"] }
```

```rust
// your own hooks, matching your executor version exactly
#[no_mangle]
extern "Rust" fn _embassy_trace_task_exec_begin(_e: u32, task_id: u32) {
    embassy_cpu_usage::task_begin(task_id);
}
#[no_mangle]
extern "Rust" fn _embassy_trace_task_exec_end(_e: u32, _t: u32) {
    embassy_cpu_usage::task_end();
}
// ...plus empty stubs for any other _embassy_trace_* symbols the linker names.
```

## Notes

- Overhead is two timer reads plus a short critical section per task poll. Fine
  for measurement builds; drop the `trace` feature on `embassy-executor` for
  production.
- `platform-cortex-m` is documented as single-core-only, but works across both
  RP2350 cores because RP's SIO wires the inter-core event signal (SEV on one
  core breaks WFE on the other). No special handling needed.
- Core index is read from `SIO.CPUID` at `0xd000_0000`, which is valid on both
  RP2040 and RP2350, so the crate has no dependency on a specific HAL version.

## License

MIT OR Apache-2.0
