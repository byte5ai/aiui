fn main() {
    tauri_build::build();

    // Build timestamp (ISO 8601, UTC)
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    println!("cargo:rustc-env=AIUI_BUILD_TIMESTAMP={now}");

    // Git SHA, shortened, "nogit" if not in a git repo
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "nogit".into());
    println!("cargo:rustc-env=AIUI_GIT_SHA={sha}");

    // Rebuild when HEAD moves
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
