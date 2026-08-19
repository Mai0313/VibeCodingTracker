use std::process::Command;

fn main() {
    let base_version = env!("CARGO_PKG_VERSION");
    let git_version = get_git_version(base_version);
    println!("cargo:rustc-env=BUILD_VERSION={}", git_version);

    let rust_version = get_rust_version();
    let cargo_version = get_cargo_version();
    println!("cargo:rustc-env=BUILD_RUST_VERSION={}", rust_version);
    println!("cargo:rustc-env=BUILD_CARGO_VERSION={}", cargo_version);

    // Emitting any `rerun-if-changed` opts out of Cargo's default "re-run when
    // any package file changes", so the stamped version refreshes when HEAD or
    // the index moves rather than on every source edit. This crate is a
    // workspace member, so `<crate>/.git` does not exist; resolve the
    // repository's real git dir instead (a file->dir indirection under a git
    // worktree).
    if let Some(git_dir) = git_dir() {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/index");
    }
}

/// Absolute path to the repository's git dir (worktree-aware), or `None`
/// outside a git checkout (e.g. a packaged `.crate`).
fn git_dir() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let dir = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!dir.is_empty()).then_some(dir)
}

/// Assembles `<tag>[-<commits>]-g<sha>[-dirty]`, substituting `base_version`
/// for a missing tag and returning it alone outside a git checkout.
fn get_git_version(base_version: &str) -> String {
    if !is_git_repo() {
        return base_version.to_string();
    }

    let latest_tag = get_latest_tag();

    let version = if let Some(ref tag) = latest_tag {
        tag.strip_prefix('v').unwrap_or(tag).to_string()
    } else {
        base_version.to_string()
    };

    let mut version_parts = vec![version];

    let commit_count = if let Some(ref tag) = latest_tag {
        get_commit_count_from_tag(tag)
    } else {
        get_total_commit_count()
    };

    if commit_count > 0 {
        version_parts.push(commit_count.to_string());
    }

    if let Some(commit_hash) = get_commit_hash() {
        version_parts.push(format!("g{}", commit_hash));
    }

    if is_working_directory_dirty() {
        version_parts.push("dirty".to_string());
    }

    version_parts.join("-")
}

fn is_git_repo() -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn get_latest_tag() -> Option<String> {
    Command::new("git")
        .args(["describe", "--tags", "--abbrev=0"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
}

fn get_commit_count_from_tag(tag: &str) -> usize {
    Command::new("git")
        .args(["rev-list", &format!("{}..HEAD", tag), "--count"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn get_total_commit_count() -> usize {
    Command::new("git")
        .args(["rev-list", "HEAD", "--count"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn get_commit_hash() -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
}

fn is_working_directory_dirty() -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|output| {
            if output.status.success() {
                !output.stdout.is_empty()
            } else {
                false
            }
        })
        .unwrap_or(false)
}

fn get_rust_version() -> String {
    Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            String::from_utf8(output.stdout).ok().and_then(|s| {
                // Extract version number from "rustc 1.28.2 (xxxxx)"
                s.split_whitespace().nth(1).map(|v| v.to_string())
            })
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn get_cargo_version() -> String {
    Command::new("cargo")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            String::from_utf8(output.stdout).ok().and_then(|s| {
                // Extract version number from "cargo 1.89.0 (xxxxx)"
                s.split_whitespace().nth(1).map(|v| v.to_string())
            })
        })
        .unwrap_or_else(|| "unknown".to_string())
}
