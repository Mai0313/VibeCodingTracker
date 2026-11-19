//! glibc allocator controls that bound resident-set retention.
//!
//! Both functions are no-ops outside Linux/glibc.

/// Asks glibc to return free arena pages to the OS (`malloc_trim(0)`).
///
/// Cost is O(arena size), suited to cycle boundaries rather than hot loops. No-op outside Linux/glibc.
#[inline]
pub fn release_freed_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` is MT-Safe, takes no pointers, and only inspects allocator free lists to release unused pages.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Applies one-time glibc `mallopt` tuning; no-op outside Linux/glibc.
///
/// Must run before multithreaded allocations begin for `M_ARENA_MAX` to take full effect and to satisfy glibc's `MT-Unsafe const:mallopt` contract.
///
/// - `M_ARENA_MAX = 4`: caps per-thread arenas so freed pages do not accumulate independently across worker threads.
/// - `M_TRIM_THRESHOLD = 128 KiB`: pins the top-chunk trim threshold instead of letting glibc's dynamic threshold grow over long runs.
pub fn tune_system_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // Stable glibc ABI constants from <malloc.h>, not re-exported by `libc`.
        const M_TRIM_THRESHOLD: libc::c_int = -1;
        const M_ARENA_MAX: libc::c_int = -8;
        // SAFETY: `M_ARENA_MAX` and `M_TRIM_THRESHOLD` are valid glibc `mallopt` options taking integer values and no pointers; glibc marks `mallopt` MT-Unsafe (`const:mallopt`), so callers must invoke this once before spawning allocating threads.
        unsafe {
            libc::mallopt(M_ARENA_MAX, 4);
            libc::mallopt(M_TRIM_THRESHOLD, 128 * 1024);
        }
    }
}
