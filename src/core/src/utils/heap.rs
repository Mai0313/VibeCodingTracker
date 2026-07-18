//! Allocator-level helpers.
//!
//! The TUI refresh loops allocate and drop large numbers of small objects
//! each cycle (session-file JSONL parsers, per-model hashmaps, ratatui row
//! vectors). With the default glibc allocator this leaves arenas full of
//! freed-but-not-returned pages, which is what drives the monotonic RSS
//! growth you see on a long-running `vibe_coding_tracker usage` session.
//!
//! `release_freed_heap` calls `malloc_trim(0)` on Linux/glibc to ask the
//! allocator to give those pages back to the kernel. It is a no-op on
//! other platforms (musl, macOS, Windows) because the symbol isn't
//! available — those allocators either return memory eagerly already or
//! don't expose a trim knob.

/// Ask the system allocator to release any free pages in its arenas back
/// to the OS. Safe to call as often as you like — cost is O(arena size).
#[inline]
pub fn release_freed_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` is a pure advisory call that inspects the
    // allocator's free lists and returns unused pages. It has no
    // preconditions and returns 1 if memory was released, 0 otherwise.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Apply one-time glibc malloc tuning. Must be called before the first
/// allocation that crosses thread boundaries to have its full effect.
///
/// What it does (Linux glibc only; no-op elsewhere):
///
/// - `M_ARENA_MAX = 2`: cap the number of per-thread arenas glibc will
///   create for multi-threaded workloads. Without this cap, a 16-core box
///   can spin up to 128 arenas for our Rayon worker pool; each arena
///   retains its own free list independently of `malloc_trim`, which is
///   how the TUI grew ~6 MB per 10 s refresh even after we trimmed the
///   main arena at the end of every cycle. Two arenas is enough to keep
///   allocator lock contention off the critical path while preventing the
///   retention from multiplying across cores.
/// - `M_TRIM_THRESHOLD = 128 KiB`: lower the threshold at which glibc
///   will voluntarily hand the arena's top chunk back to the OS. Default
///   (128 KiB) is already low but the value can grow automatically; we
///   pin it so long sessions don't drift.
pub fn tune_system_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // Constants are stable glibc ABI but not re-exported by the `libc`
        // crate; see `malloc.h`: M_TRIM_THRESHOLD = -1, M_ARENA_MAX = -8.
        const M_TRIM_THRESHOLD: libc::c_int = -1;
        const M_ARENA_MAX: libc::c_int = -8;
        // SAFETY: `mallopt` is documented as thread-safe and has no
        // preconditions; invalid option numbers simply return 0.
        unsafe {
            libc::mallopt(M_ARENA_MAX, 2);
            libc::mallopt(M_TRIM_THRESHOLD, 128 * 1024);
        }
    }
}
