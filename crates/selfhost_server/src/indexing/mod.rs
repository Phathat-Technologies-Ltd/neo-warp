//! Codebase indexing: answers the client's Merkle tree sync and retrieval operations from a local
//! content-addressed store, searched with zvec-grep when it is installed and a built-in BM25
//! ranker otherwise.

mod lexical;
mod store;
mod zvec;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use store::IndexStore;
use zvec::ZvecGrep;

/// How often, in seconds, the client syncs changes to an index.
const EMBEDDING_CADENCE_SECS: i32 = 300;
/// The embedding config the client is told to use. It only namespaces stored indexes here.
const EMBEDDING_CONFIG: &str = "VOYAGE_CODE_3_512";
const MAX_RELEVANT_FRAGMENTS: usize = 40;
const MAX_RELEVANT_FILES: usize = 10;
/// zvec-grep searches the whole config's workspace, so ask for extra hits to have enough left
/// after keeping only the requested root's fragments.
const ZVEC_OVERFETCH: usize = 4;

/// A GraphQL operation answered by [`CodebaseIndex`].
#[derive(Clone, Copy, Debug)]
pub enum IndexOperation {
    CodebaseContextConfig,
    SyncMerkleTree,
    UpdateMerkleTree,
    GenerateCodeEmbeddings,
    PopulateMerkleTreeCache,
    GetRelevantFragments,
    RerankFragments,
}

impl IndexOperation {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "CodebaseContextConfigQuery" => Self::CodebaseContextConfig,
            "SyncMerkleTree" => Self::SyncMerkleTree,
            "UpdateMerkleTree" => Self::UpdateMerkleTree,
            "GenerateCodeEmbeddings" => Self::GenerateCodeEmbeddings,
            "PopulateMerkleTreeCache" => Self::PopulateMerkleTreeCache,
            "GetRelevantFragmentsQuery" => Self::GetRelevantFragments,
            "RerankFragments" => Self::RerankFragments,
            _ => return None,
        })
    }
}

pub struct CodebaseIndex {
    store: Mutex<IndexStore>,
    zvec_grep: Option<ZvecGrep>,
}

impl CodebaseIndex {
    /// An index stored under `root`, searched with zvec-grep's default binary.
    pub fn new(root: PathBuf) -> Self {
        Self::with_zvec_grep(root, Some(ZvecGrep::new(zvec::default_binary())))
    }

    /// An index searched with the `zg` at `binary`, or only with the built-in ranker for `None`.
    pub fn with_zvec_grep_binary(root: PathBuf, binary: Option<std::ffi::OsString>) -> Self {
        Self::with_zvec_grep(root, binary.map(ZvecGrep::new))
    }

    fn with_zvec_grep(root: PathBuf, zvec_grep: Option<ZvecGrep>) -> Self {
        Self {
            store: Mutex::new(IndexStore::new(root)),
            zvec_grep,
        }
    }

    /// Answers `operation` for user `uid` from its GraphQL `variables`, as a GraphQL response.
    pub async fn respond(
        self: &Arc<Self>,
        operation: IndexOperation,
        uid: String,
        variables: Value,
    ) -> Value {
        let index = self.clone();
        tokio::task::spawn_blocking(move || index.respond_blocking(operation, &uid, variables))
            .await
            .unwrap_or_else(|error| graphql_error(&format!("indexing failed: {error}")))
    }

    fn respond_blocking(&self, operation: IndexOperation, uid: &str, variables: Value) -> Value {
        let result = match operation {
            IndexOperation::CodebaseContextConfig => Ok(json!({
                "codebaseContextConfig": {
                    "__typename": "CodebaseContextConfigOutput",
                    "embeddingCadence": EMBEDDING_CADENCE_SECS,
                    "embeddingConfig": EMBEDDING_CONFIG,
                },
            })),
            IndexOperation::SyncMerkleTree => parse(variables)
                .and_then(|vars| self.sync_merkle_tree(uid, vars))
                .map(|changed| {
                    json!({ "syncMerkleTree": output("SyncMerkleTreeOutput", json!({
                        "changedNodes": changed,
                    })) })
                })
                .or_else(|error| typed_error("syncMerkleTree", "SyncMerkleTreeError", error)),
            IndexOperation::UpdateMerkleTree => parse(variables)
                .and_then(|vars| self.update_merkle_tree(uid, vars))
                .map(|results| {
                    json!({ "updateMerkleTree": output("UpdateMerkleTreeOutput", json!({
                        "results": results,
                    })) })
                })
                .or_else(|error| typed_error("updateMerkleTree", "UpdateMerkleTreeError", error)),
            IndexOperation::GenerateCodeEmbeddings => parse(variables)
                .and_then(|vars| self.generate_code_embeddings(uid, vars))
                .map(|results| {
                    json!({ "generateCodeEmbeddings": output("GenerateCodeEmbeddingsOutput", json!({
                        "embeddingResults": results,
                    })) })
                })
                .or_else(|error| {
                    typed_error(
                        "generateCodeEmbeddings",
                        "GenerateCodeEmbeddingsError",
                        error,
                    )
                }),
            IndexOperation::PopulateMerkleTreeCache => parse(variables)
                .and_then(|vars| self.populate_merkle_tree_cache(uid, vars))
                .map(|success| {
                    json!({ "populateMerkleTreeCache": {
                        "__typename": "PopulateMerkleTreeCacheOutput",
                        "success": success,
                    } })
                }),
            IndexOperation::GetRelevantFragments => parse(variables)
                .and_then(|vars| self.get_relevant_fragments(uid, vars))
                .map(|hashes| {
                    json!({ "getRelevantFragments": {
                        "__typename": "GetRelevantFragmentsOutput",
                        "candidateHashes": hashes,
                    } })
                })
                .or_else(|error| {
                    typed_error("getRelevantFragments", "GetRelevantFragmentsError", error)
                }),
            IndexOperation::RerankFragments => parse(variables)
                .map(rerank_fragments)
                .map(|ranked| {
                    json!({ "rerankFragments": {
                        "__typename": "RerankFragmentsOutput",
                        "rankedFragments": ranked,
                    } })
                })
                .or_else(|error| typed_error("rerankFragments", "RerankFragmentsError", error)),
        };
        match result {
            Ok(data) => json!({ "data": data }),
            Err(error) => {
                log::warn!("Codebase indexing {operation:?} failed: {error:#}");
                graphql_error(&format!("{error:#}"))
            }
        }
    }

    fn sync_merkle_tree(&self, uid: &str, vars: SyncVariables) -> anyhow::Result<Vec<String>> {
        let mut store = self.store.lock().expect("index store lock");
        let namespace = store.namespace(uid, &vars.input.embedding_config)?;
        Ok(vars
            .input
            .hashed_nodes
            .into_iter()
            .filter(|hash| !namespace.knows(hash))
            .collect())
    }

    fn update_merkle_tree(&self, uid: &str, vars: UpdateVariables) -> anyhow::Result<Vec<Value>> {
        let mut store = self.store.lock().expect("index store lock");
        let namespace = store.namespace(uid, &vars.input.embedding_config)?;
        vars.input
            .nodes
            .into_iter()
            .map(|node| {
                let success = namespace.add_node(&node.hash, node.children)?;
                Ok(json!({ "hash": node.hash, "success": success }))
            })
            .collect()
    }

    fn generate_code_embeddings(
        &self,
        uid: &str,
        vars: EmbeddingsVariables,
    ) -> anyhow::Result<Vec<Value>> {
        let mut store = self.store.lock().expect("index store lock");
        let namespace = store.namespace(uid, &vars.input.embedding_config)?;
        Ok(vars
            .input
            .fragments
            .into_iter()
            .map(|fragment| {
                let success =
                    match namespace.add_fragment(&fragment.content_hash, &fragment.content) {
                        Ok(()) => true,
                        Err(error) => {
                            log::warn!("Couldn't store a code fragment: {error:#}");
                            false
                        }
                    };
                json!({ "hash": fragment.content_hash, "success": success })
            })
            .collect())
    }

    fn populate_merkle_tree_cache(&self, uid: &str, vars: RootVariables) -> anyhow::Result<bool> {
        let mut store = self.store.lock().expect("index store lock");
        Ok(store
            .namespace(uid, &vars.embedding_config)?
            .knows(&vars.root_hash))
    }

    fn get_relevant_fragments(
        &self,
        uid: &str,
        vars: RelevantFragmentsVariables,
    ) -> anyhow::Result<Vec<String>> {
        let mut store = self.store.lock().expect("index store lock");
        let namespace = store.namespace(uid, &vars.embedding_config)?;
        let candidates = namespace.fragments_under(&vars.root_hash);
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(zvec_grep) = &self.zvec_grep {
            let workspace = namespace.fragments_dir();
            let searched =
                (!namespace.needs_reindex || zvec_grep.index(&workspace).is_ok()).then(|| {
                    namespace.needs_reindex = false;
                    zvec_grep.search(
                        &workspace,
                        &vars.query,
                        MAX_RELEVANT_FRAGMENTS * ZVEC_OVERFETCH,
                    )
                });
            match searched {
                Some(Ok(hits)) => {
                    let candidates: std::collections::HashSet<_> = candidates.iter().collect();
                    return Ok(hits
                        .into_iter()
                        .filter(|hash| candidates.contains(hash))
                        .take(MAX_RELEVANT_FRAGMENTS)
                        .collect());
                }
                Some(Err(error)) => {
                    log::debug!("zvec-grep search failed, using the built-in ranker: {error:#}");
                }
                None => log::debug!("zvec-grep indexing failed, using the built-in ranker"),
            }
        }
        let contents: Vec<String> = candidates
            .iter()
            .map(|hash| namespace.fragment(hash).unwrap_or_default())
            .collect();
        let documents: Vec<&str> = contents.iter().map(String::as_str).collect();
        Ok(lexical::rank(&vars.query, &documents)
            .into_iter()
            .take(MAX_RELEVANT_FRAGMENTS)
            .map(|index| candidates[index].clone())
            .collect())
    }
}

/// Orders fragments by relevance to the query, matches first, keeping every fragment.
fn rerank_fragments(vars: RerankVariables) -> Vec<Value> {
    let documents: Vec<String> = vars
        .fragments
        .iter()
        .map(|fragment| {
            format!(
                "{} {}",
                fragment.location["filePath"].as_str().unwrap_or_default(),
                fragment.content
            )
        })
        .collect();
    let documents: Vec<&str> = documents.iter().map(String::as_str).collect();
    let mut order = lexical::rank(&vars.query, &documents);
    let ranked: std::collections::HashSet<usize> = order.iter().copied().collect();
    order.extend((0..vars.fragments.len()).filter(|index| !ranked.contains(index)));
    order
        .into_iter()
        .map(|index| {
            let fragment = &vars.fragments[index];
            json!({
                "content": fragment.content,
                "contentHash": fragment.content_hash,
                "location": fragment.location,
            })
        })
        .collect()
}

/// The paths of `files` most relevant to `query`, by path and symbols.
pub fn relevant_files(query: &str, files: &[(String, String)]) -> Vec<String> {
    let documents: Vec<String> = files
        .iter()
        .map(|(path, symbols)| format!("{path} {symbols}"))
        .collect();
    let documents: Vec<&str> = documents.iter().map(String::as_str).collect();
    lexical::rank(query, &documents)
        .into_iter()
        .take(MAX_RELEVANT_FILES)
        .map(|index| files[index].0.clone())
        .collect()
}

fn parse<T: DeserializeOwned>(variables: Value) -> anyhow::Result<T> {
    Ok(serde_json::from_value(variables)?)
}

fn output(typename: &str, mut fields: Value) -> Value {
    fields["__typename"] = json!(typename);
    fields["responseContext"] = json!({ "serverVersion": null });
    fields
}

fn typed_error(field: &str, typename: &str, error: anyhow::Error) -> anyhow::Result<Value> {
    log::warn!("Codebase indexing {field} failed: {error:#}");
    Ok(json!({ field: { "__typename": typename, "error": format!("{error:#}") } }))
}

fn graphql_error(message: &str) -> Value {
    json!({ "data": null, "errors": [{ "message": message }] })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncVariables {
    input: SyncInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncInput {
    hashed_nodes: Vec<String>,
    embedding_config: String,
}

#[derive(Deserialize)]
struct UpdateVariables {
    input: UpdateInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInput {
    embedding_config: String,
    nodes: Vec<NodeInput>,
}

#[derive(Deserialize)]
struct NodeInput {
    hash: String,
    children: Vec<String>,
}

#[derive(Deserialize)]
struct EmbeddingsVariables {
    input: EmbeddingsInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmbeddingsInput {
    embedding_config: String,
    fragments: Vec<FragmentInput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FragmentInput {
    content: String,
    content_hash: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RootVariables {
    embedding_config: String,
    root_hash: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelevantFragmentsVariables {
    embedding_config: String,
    root_hash: String,
    query: String,
}

#[derive(Deserialize)]
struct RerankVariables {
    fragments: Vec<RerankFragmentInput>,
    query: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RerankFragmentInput {
    content: String,
    content_hash: String,
    location: Value,
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
