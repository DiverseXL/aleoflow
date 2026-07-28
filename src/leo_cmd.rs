use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run a `leo` subcommand, optionally inside a specific project directory.
///
/// Inherits stdout/stderr so the user sees Leo's real output directly.
/// Returns a descriptive error if `leo` is not found or the subcommand fails.
pub fn run_leo(subcommand: &str, dir: Option<&Path>) -> Result<()> {
    run_leo_with(subcommand, &[], dir)
}

/// Run a `leo` subcommand with extra flags, optionally in a project directory.
pub fn run_leo_with(subcommand: &str, extra_flags: &[String], dir: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new("leo");
    cmd.arg(subcommand);
    for flag in extra_flags {
        cmd.arg(flag);
    }

    if let Some(path) = dir {
        cmd.current_dir(path);
    }

    let status = cmd.status().with_context(|| {
        format!(
            "Failed to execute 'leo {}'. \
             Make sure leo is installed and on your PATH.\n\
             Install it with: cargo binstall leo-lang",
            subcommand
        )
    })?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("'leo {}' failed with exit code {}", subcommand, code);
    }

    Ok(())
}

/// Check whether `leo` is available on PATH.
pub fn leo_is_installed() -> bool {
    Command::new("leo")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build the `--json-output[=<PATH>]` flags based on the user's setting.
/// Returns an empty vec if the flag was not passed.
pub fn json_output_flag(json_output: &Option<Option<PathBuf>>) -> Vec<String> {
    match json_output {
        Some(None) => vec!["--json-output".to_string()],
        Some(Some(path)) => {
            vec![format!("--json-output={}", path.to_string_lossy())]
        }
        None => vec![],
    }
}
