//! Runs zvec-grep's `zg` CLI over a fragment workspace.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Child, Stdio};
use std::thread;
use std::time::Duration;

use command::blocking::Command;
use instant::Instant;

use anyhow::{Context as _, bail};

use super::store::is_valid_hash;

/// The `zg` binary: `ZVEC_GREP_BIN` if set, otherwise `zg` on `PATH`.
pub fn default_binary() -> OsString {
    std::env::var_os("ZVEC_GREP_BIN").unwrap_or_else(|| "zg".into())
}

/// How long `zg --index` may run before it's killed and the built-in ranker answers instead.
const INDEX_TIMEOUT: Duration = Duration::from_secs(120);
/// How long `zg --hybrid` may run before it's killed and the built-in ranker answers instead.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

pub struct ZvecGrep {
    binary: OsString,
    index_timeout: Duration,
    search_timeout: Duration,
}

impl ZvecGrep {
    pub fn new(binary: OsString) -> Self {
        Self::with_timeouts(binary, INDEX_TIMEOUT, SEARCH_TIMEOUT)
    }

    pub fn with_timeouts(
        binary: OsString,
        index_timeout: Duration,
        search_timeout: Duration,
    ) -> Self {
        Self {
            binary,
            index_timeout,
            search_timeout,
        }
    }

    /// Builds or refreshes zvec-grep's index of `workspace`.
    pub fn index(&self, workspace: &Path) -> anyhow::Result<()> {
        self.run(
            workspace,
            &["--index", "--color", "never"],
            self.index_timeout,
        )
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
            self.search_timeout,
        )?;
        Ok(fragment_hashes_in_output(&output))
    }

    fn run(&self, workspace: &Path, args: &[&str], timeout: Duration) -> anyhow::Result<String> {
        let mut child = Command::new(&self.binary)
            .args(args)
            .current_dir(workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("couldn't run {}", self.binary.to_string_lossy()))?;
        // Drained on their own threads so a chatty child can't block on a full pipe.
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let status = wait_with_timeout(&mut child, timeout)?;
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        if !status.success() {
            bail!("zg exited with {status}: {}", stderr.trim());
        }
        Ok(stdout)
    }
}

fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn wait_with_timeout(
    child: &mut Child,
    timeout: Duration,
) -> anyhow::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("zg timed out after {timeout:?}");
        }
        thread::sleep(POLL_INTERVAL);
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
