//! The on-disk, content-addressed store of Merkle tree nodes and fragments.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const NODES_FILE: &str = "nodes.jsonl";
const FRAGMENTS_DIR: &str = "fragments";

/// Whether `hash` has the client's hash format: 64 lowercase hex characters. Only valid hashes
/// become file names.
pub fn is_valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[derive(Serialize, Deserialize)]
struct StoredNode {
    hash: String,
    children: Vec<String>,
}

/// One user's index for one embedding config.
pub struct Namespace {
    dir: PathBuf,
    nodes: HashMap<String, Vec<String>>,
    fragments: HashSet<String>,
    /// Whether fragments were added since zvec-grep last indexed the workspace.
    pub needs_reindex: bool,
}

impl Namespace {
    fn open(dir: PathBuf) -> anyhow::Result<Self> {
        let fragments_dir = dir.join(FRAGMENTS_DIR);
        fs::create_dir_all(&fragments_dir)
            .with_context(|| format!("couldn't create {}", fragments_dir.display()))?;
        let mut nodes = HashMap::new();
        match fs::File::open(dir.join(NODES_FILE)) {
            Ok(file) => {
                for line in BufReader::new(file).lines() {
                    // A line cut short by a crash is skipped; the client re-sends its node.
                    if let Ok(node) = serde_json::from_str::<StoredNode>(&line?) {
                        nodes.insert(node.hash, node.children);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut fragments = HashSet::new();
        for entry in fs::read_dir(&fragments_dir)? {
            let name = entry?.file_name();
            if let Some(hash) = name.to_str().and_then(|name| name.strip_suffix(".txt"))
                && is_valid_hash(hash)
            {
                fragments.insert(hash.to_owned());
            }
        }
        Ok(Self {
            dir,
            nodes,
            fragments,
            needs_reindex: true,
        })
    }

    /// The zvec-grep workspace holding one `<hash>.txt` file per fragment.
    pub fn fragments_dir(&self) -> PathBuf {
        self.dir.join(FRAGMENTS_DIR)
    }

    pub fn knows(&self, hash: &str) -> bool {
        self.nodes.contains_key(hash) || self.fragments.contains(hash)
    }

    /// Stores an intermediate node if all its children are known, returning whether it did.
    pub fn add_node(&mut self, hash: &str, children: Vec<String>) -> anyhow::Result<bool> {
        if !is_valid_hash(hash) || !children.iter().all(|child| self.knows(child)) {
            return Ok(false);
        }
        if self.nodes.get(hash) == Some(&children) {
            return Ok(true);
        }
        let mut line = serde_json::to_string(&StoredNode {
            hash: hash.to_owned(),
            children: children.clone(),
        })?;
        line.push('\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(NODES_FILE))?
            .write_all(line.as_bytes())?;
        self.nodes.insert(hash.to_owned(), children);
        Ok(true)
    }

    pub fn add_fragment(&mut self, hash: &str, content: &str) -> anyhow::Result<()> {
        if !is_valid_hash(hash) {
            bail!("invalid fragment hash");
        }
        if self.fragments.contains(hash) {
            return Ok(());
        }
        let path = self.fragment_path(hash);
        let partial = path.with_extension("partial");
        fs::write(&partial, content)?;
        fs::rename(&partial, &path)?;
        self.fragments.insert(hash.to_owned());
        self.needs_reindex = true;
        Ok(())
    }

    pub fn fragment(&self, hash: &str) -> Option<String> {
        if !self.fragments.contains(hash) {
            return None;
        }
        fs::read_to_string(self.fragment_path(hash)).ok()
    }

    fn fragment_path(&self, hash: &str) -> PathBuf {
        self.fragments_dir().join(format!("{hash}.txt"))
    }

    /// The fragments reachable from `root`, in tree order.
    pub fn fragments_under(&self, root: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![root.to_owned()];
        while let Some(hash) = stack.pop() {
            if !seen.insert(hash.clone()) {
                continue;
            }
            if let Some(children) = self.nodes.get(&hash) {
                stack.extend(children.iter().rev().cloned());
            } else if self.fragments.contains(&hash) {
                found.push(hash);
            }
        }
        found
    }
}

/// Every user's namespaces under one root directory.
pub struct IndexStore {
    root: PathBuf,
    namespaces: HashMap<(String, String), Namespace>,
}

impl IndexStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            namespaces: HashMap::new(),
        }
    }

    pub fn namespace(
        &mut self,
        uid: &str,
        embedding_config: &str,
    ) -> anyhow::Result<&mut Namespace> {
        let key = (uid.to_owned(), embedding_config.to_owned());
        if !self.namespaces.contains_key(&key) {
            let dir = namespace_dir(&self.root, uid, embedding_config)?;
            self.namespaces.insert(key.clone(), Namespace::open(dir)?);
        }
        Ok(self.namespaces.get_mut(&key).expect("inserted above"))
    }
}

fn namespace_dir(root: &Path, uid: &str, embedding_config: &str) -> anyhow::Result<PathBuf> {
    if embedding_config.is_empty()
        || !embedding_config
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        bail!("invalid embedding config");
    }
    let user = hex::encode(&Sha256::digest(uid.as_bytes())[..8]);
    Ok(root.join(user).join(embedding_config.to_ascii_lowercase()))
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
