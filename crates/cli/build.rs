use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Force this build script to re-run on every `cargo build` so the
    // version string (derived from `git describe --dirty`) stays fresh
    // even for uncommitted working-tree changes. We bump a marker
    // file's mtime and register it via `rerun-if-changed`; cargo sees
    // the mtime changed since the last build and re-runs the script.
    let marker = std::path::Path::new("target/.version-marker");
    let _ = fs::create_dir_all("target");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .to_string();
    let _ = fs::write(marker, stamp);
    println!("cargo:rerun-if-changed={}", marker.display());

    // Run `git describe --always --dirty` to get version string.
    // Output example: `60ae860` (clean) or `60ae860-dirty` (uncommitted changes).
    let git_version = Command::new("git")
        .args(["describe", "--always", "--dirty"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".into());

    println!("cargo:rustc-env=GIT_VERSION={git_version}");
}
