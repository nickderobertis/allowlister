//! Deterministic allocation report for the engine hot paths.
//!
//! Not a statistical benchmark: a counting global allocator tallies allocator
//! calls and requested bytes for one `analyze`, `decide`, and `evaluate` per
//! corpus command (plus one example-config load), then prints a markdown table.
//! The counts are exact and stable for a given commit, so two runs are directly
//! comparable — in CI or by eye — without warmups or statistics. They surface
//! allocator pressure, which the wall-clock numbers in `benches/engine.rs`
//! cannot attribute.
//!
//! `harness = false` with a plain `main` keeps libtest, Criterion, nextest, and
//! coverage away from this target (it is measured, not gated). The `--bench`
//! argument cargo passes is deliberately ignored.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};

use allowlister::config;
use allowlister::domain;

#[path = "support/mod.rs"]
mod support;

/// The system allocator wrapped with relaxed atomic tallies. A `realloc`
/// counts as one call plus only the grown bytes, so `BYTES` tracks total
/// memory requested without double-counting moves; frees are not tracked.
struct CountingAlloc;

static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        CALLS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        CALLS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(
            new_size.saturating_sub(layout.size()) as u64,
            Ordering::Relaxed,
        );
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Allocator calls and bytes requested while running `f` (including dropping
/// its result).
fn measure<T>(f: impl FnOnce() -> T) -> (u64, u64) {
    let calls = CALLS.load(Ordering::Relaxed);
    let bytes = BYTES.load(Ordering::Relaxed);
    black_box(f());
    (
        CALLS.load(Ordering::Relaxed) - calls,
        BYTES.load(Ordering::Relaxed) - bytes,
    )
}

fn main() {
    let rules = support::example_rules();

    // Flush lazy one-time initialization (parser tables, interned statics) out
    // of the measured calls, so every row reflects steady-state cost.
    for (_, cmd) in support::corpus() {
        black_box(domain::evaluate(&cmd, &rules));
    }

    println!("| operation | case | allocator calls | bytes requested |");
    println!("|---|---|---:|---:|");
    for (name, cmd) in support::corpus() {
        let (calls, bytes) = measure(|| domain::analyze(&cmd));
        println!("| analyze | {name} | {calls} | {bytes} |");

        let analysis = domain::analyze(&cmd);
        let (calls, bytes) = measure(|| domain::decide(&analysis, &rules));
        println!("| decide | {name} | {calls} | {bytes} |");

        let (calls, bytes) = measure(|| domain::evaluate(&cmd, &rules));
        println!("| evaluate | {name} | {calls} | {bytes} |");
    }

    let paths = support::example_config_paths();
    let (calls, bytes) = measure(|| config::load_from_paths(&paths));
    println!("| config_load | examples | {calls} | {bytes} |");
}
