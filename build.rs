use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_string())
}

fn main() {
    // Reavaliar tanto a revisão quanto alterações no conteúdo incluído no binário.
    for path in ["src", "www", "Cargo.toml", "Cargo.lock", "build.rs"] {
        println!("cargo:rerun-if-changed={path}");
    }
    for path in ["HEAD", "index"] {
        if let Some(path) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={path}");
    }

    if let Some(commit) = git(&["rev-parse", "--verify", "HEAD"])
        && matches!(commit.len(), 40 | 64)
        && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        println!("cargo:rustc-env=HOST_STATUS_COMMIT={commit}");
    }
    if let Some(status) = git(&["status", "--porcelain", "--untracked-files=normal"]) {
        println!("cargo:rustc-env=HOST_STATUS_DIRTY={}", !status.is_empty());
    }
}
