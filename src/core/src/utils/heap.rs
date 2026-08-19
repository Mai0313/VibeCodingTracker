//! Allocator-level helpers that bound the TUI's resident-set growth.
//!
//! Each refresh cycle allocates and drops large numbers of small objects, and
//! the default glibc allocator retains those as freed-but-not-returned pages.
//! Both helpers below exist solely to stop that retention accumulating; both
//! are no-ops outside Linux/glibc, where neither symbol exists.

/// Asks the system allocator to return the free pages in its arenas to the OS.
///
/// Called after each successful TUI refresh. Cost is O(arena size), so it
/// suits a cycle boundary rather than a hot loop. No-op outside Linux/glibc.
#[inline]
pub fn release_freed_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` is advisory and has no preconditions; it only
    // inspects the allocator's free lists and releases unused pages.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Applies one-time glibc malloc tuning; no-op outside Linux/glibc.
///
/// Must run before the first allocation that crosses thread boundaries to
/// have its full effect. Both knobs are load-bearing, not cosmetic:
///
/// - `M_ARENA_MAX = 2` caps the per-thread arenas glibc creates for the Rayon
///   pool (a 16-core box otherwise gets up to 128). Each arena retains its own
///   free list independently of `malloc_trim`, which is how the TUI grew
///   ~6 MB per 10 s refresh even with a trim at the end of every cycle. Two
///   arenas keep allocator lock contention off the critical path without
///   multiplying that retention across cores.
/// - `M_TRIM_THRESHOLD = 128 KiB` pins the point at which glibc hands the
///   arena's top chunk back to the OS. The default starts at this value but
///   grows automatically, so long sessions drift without the pin.
pub fn tune_system_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // Stable glibc ABI, but not re-exported by the `libc` crate; the
        // values come from `malloc.h`.
        const M_TRIM_THRESHOLD: libc::c_int = -1;
        const M_ARENA_MAX: libc::c_int = -8;
        // SAFETY: `mallopt` is thread-safe and has no preconditions; an
        // unrecognized option number simply returns 0.
        unsafe {
            libc::mallopt(M_ARENA_MAX, 2);
            libc::mallopt(M_TRIM_THRESHOLD, 128 * 1024);
        }
    }
}
