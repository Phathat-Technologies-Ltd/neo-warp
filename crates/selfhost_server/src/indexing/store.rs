//! The on-disk, content-addressed store of Merkle tree nodes and fragments.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const NODES_FILE: &str = "nodes.jsonl";
const FRAGMENTS_DIR: &str = "fragments";
/// Where fragments are written before being renamed into place, outside the zvec-grep workspace
/// so a partial write is never indexed.
const INCOMING_DIR: &str = "incoming";
/// Fragment contents kept in memory per namespace for the built-in ranker, so it doesn't re-read
/// every fragment from disk on each query.
const MAX_CACHED_BYTES: usize = 64 * 1024 * 1024;

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
    cache: HashMap<String, Arc<str>>,
    cached_bytes: usize,
    /// Incremented whenever a fragment is added.
    generation: u64,
    /// The `generation` zvec-grep last indexed, if any.
    indexed_generation: Option<u64>,
}

impl Namespace {
    fn open(dir: PathBuf) -> anyhow::Result<Self> {
        let fragments_dir = dir.join(FRAGMENTS_DIR);
        fs::create_dir_all(&fragments_dir)
            .with_context(|| format!("couldn't create {}", fragments_dir.display()))?;
        let incoming_dir = dir.join(INCOMING_DIR);
        // Leftovers of writes interrupted by a crash.
        let _ = fs::remove_dir_all(&incoming_dir);
        fs::create_dir_all(&incoming_dir)
            .with_context(|| format!("couldn't create {}", incoming_dir.display()))?;
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
            cache: HashMap::new(),
            cached_bytes: 0,
            generation: 0,
            indexed_generation: None,
        })
    }

    /// The zvec-grep workspace holding one `<hash>.txt` file per fragment.
    pub fn fragments_dir(&self) -> PathBuf {
        self.dir.join(FRAGMENTS_DIR)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether fragments were added since zvec-grep last indexed the workspace.
    pub fn needs_reindex(&self) -> bool {
        self.indexed_generation != Some(self.generation)
    }

    /// Records that zvec-grep indexed the workspace as of `generation`.
    pub fn mark_indexed(&mut self, generation: u64) {
        if self
            .indexed_generation
            .is_none_or(|indexed| indexed < generation)
        {
            self.indexed_generation = Some(generation);
        }
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

    /// Stores a fragment whose `hash` must be the SHA-256 of `content`.
    pub fn add_fragment(&mut self, hash: &str, content: &str) -> anyhow::Result<()> {
        if !is_valid_hash(hash) {
            bail!("invalid fragment hash");
        }
        if hex::encode(Sha256::digest(content.as_bytes())) != hash {
            bail!("fragment hash doesn't match its content");
        }
        if self.fragments.contains(hash) {
            return Ok(());
        }
        let path = self.fragment_path(hash);
        let partial = self.dir.join(INCOMING_DIR).join(format!("{hash}.partial"));
        if let Err(error) = fs::write(&partial, content).and_then(|()| fs::rename(&partial, &path))
        {
            let _ = fs::remove_file(&partial);
            return Err(error.into());
        }
        self.fragments.insert(hash.to_owned());
        self.cache_content(hash, content.into());
        self.generation += 1;
        Ok(())
    }

    /// The content of `hash` if it's in memory, otherwise the path to read it from. Fragment files
    /// never change once written, so the path can be read without holding the namespace.
    pub fn fragment_source(&self, hash: &str) -> Option<FragmentSource> {
        if !self.fragments.contains(hash) {
            return None;
        }
        Some(match self.cache.get(hash) {
            Some(content) => FragmentSource::Cached(content.clone()),
            None => FragmentSource::File(self.fragment_path(hash)),
        })
    }

    /// Keeps `content` in memory for later queries while the cache has room.
    pub fn cache_content(&mut self, hash: &str, content: Arc<str>) {
        if self.cached_bytes + content.len() > MAX_CACHED_BYTES || self.cache.contains_key(hash) {
            return;
        }
        self.cached_bytes += content.len();
        self.cache.insert(hash.to_owned(), content);
    }

    #[cfg(test)]
    pub fn fragment(&self, hash: &str) -> Option<String> {
        match self.fragment_source(hash)? {
            FragmentSource::Cached(content) => Some(content.to_string()),
            FragmentSource::File(path) => fs::read_to_string(path).ok(),
        }
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

pub enum FragmentSource {
    Cached(Arc<str>),
    File(PathBuf),
}

/// A namespace shared between requests.
pub struct NamespaceHandle {
    pub state: Mutex<Namespace>,
    /// Held while zvec-grep runs over the namespace's workspace, so runs don't overlap while
    /// requests that only touch `state` proceed.
    pub zvec_grep: Mutex<()>,
}

/// Every user's namespaces under one root directory.
pub struct IndexStore {
    root: PathBuf,
    namespaces: HashMap<(String, String), Arc<NamespaceHandle>>,
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
    ) -> anyhow::Result<Arc<NamespaceHandle>> {
        let key = (uid.to_owned(), embedding_config.to_owned());
        if let Some(handle) = self.namespaces.get(&key) {
            return Ok(handle.clone());
        }
        let dir = namespace_dir(&self.root, uid, embedding_config)?;
        let handle = Arc::new(NamespaceHandle {
            state: Mutex::new(Namespace::open(dir)?),
            zvec_grep: Mutex::new(()),
        });
        self.namespaces.insert(key, handle.clone());
        Ok(handle)
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
