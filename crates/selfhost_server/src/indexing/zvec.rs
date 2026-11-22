//! Runs zvec-grep's `zg` CLI over a fragment workspace.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context as _, bail};

use super::store::is_valid_hash;

/// The `zg` binary: `ZVEC_GREP_BIN` if set, otherwise `zg` on `PATH`.
pub fn default_binary() -> OsString {
    std::env::var_os("ZVEC_GREP_BIN").unwrap_or_else(|| "zg".into())
}

pub struct ZvecGrep {
    binary: OsString,
}

impl ZvecGrep {
    pub fn new(binary: OsString) -> Self {
        Self { binary }
    }

    /// Builds or refreshes zvec-grep's index of `workspace`.
    pub fn index(&self, workspace: &Path) -> anyhow::Result<()> {
        self.run(workspace, &["--index", "--color", "never"])
            .map(drop)
    }

    /// Content hashes of the fragments matching `query`, best first.
    pub fn search(
        &self,
        workspace: &Path,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let limit = limit.to_string();
        let output = self.run(
            workspace,
            &[
                "--hybrid",
                query,
                "--limit",
                &limit,
                "--preview",
                "none",
                "--color",
                "never",
            ],
        )?;
        Ok(fragment_hashes_in_output(&output))
    }

    fn run(&self, workspace: &Path, args: &[&str]) -> anyhow::Result<String> {
        let output = Command::new(&self.binary)
            .args(args)
            .current_dir(workspace)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("couldn't run {}", self.binary.to_string_lossy()))?;
        if !output.status.success() {
            bail!(
                "zg exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// The fragment file names (`<hash>.txt`) in `zg`'s human-readable output, in order of first
/// appearance. Matching file names rather than the layout keeps this independent of zg's output
/// format.
pub fn fragment_hashes_in_output(output: &str) -> Vec<String> {
    let mut hashes: Vec<String> = Vec::new();
    for word in output.split(|c: char| !c.is_ascii_alphanumeric() && c != '.') {
        if let Some(hash) = word.strip_suffix(".txt")
            && is_valid_hash(hash)
            && !hashes.iter().any(|seen| seen == hash)
        {
            hashes.push(hash.to_owned());
        }
    }
    hashes
}

#[cfg(test)]
#[path = "zvec_tests.rs"]
mod tests;
