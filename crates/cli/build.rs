use std::process::Command;

/// Capture the short git SHA at build time (not at runtime) so the binary
/// carries a static "version+sha" string without spawning `git` per launch.
fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=AGENT_CODE_GIT_SHA={sha}");
    // Refresh the embedded SHA when HEAD moves.
    println!("cargo:rerun-if-changed=../.git/HEAD");
}
