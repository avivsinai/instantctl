use std::{env, path::Path, process::Command};

fn git(manifest: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(manifest)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

fn main() {
    let manifest =
        env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies the manifest directory");
    let manifest = Path::new(&manifest);
    // A source copy inside an unrelated checkout must not inherit its revision.
    let sha = git(manifest, &["ls-files", "--error-unmatch", "Cargo.toml"])
        .and_then(|_| git(manifest, &["rev-parse", "--verify", "HEAD"]))
        .filter(|sha| {
            matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
    if sha.is_some() {
        // --git-path resolves the shared refs and per-worktree HEAD correctly.
        for name in ["HEAD", "refs", "packed-refs"] {
            if let Some(path) = git(manifest, &["rev-parse", "--git-path", name]) {
                let path = manifest.join(path);
                if path.exists() {
                    println!("cargo::rerun-if-changed={}", path.display());
                }
            }
        }
    }
    println!(
        "cargo::rustc-env=INSTANTCTL_GIT_SHA={}",
        sha.as_deref().unwrap_or("unknown")
    );
}
