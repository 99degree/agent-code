use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
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
