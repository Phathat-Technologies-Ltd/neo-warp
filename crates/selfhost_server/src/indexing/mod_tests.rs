use cynic::{MutationBuilder as _, QueryBuilder as _};
use sha2::{Digest as _, Sha256};
use warp_graphql::full_source_code_embedding::{
    ContentHash, EmbeddingConfig, Fragment, NodeHash, RepoMetadata,
};
use warp_graphql::mutations::generate_code_embeddings::{
    GenerateCodeEmbeddings, GenerateCodeEmbeddingsInput, GenerateCodeEmbeddingsResult,
    GenerateCodeEmbeddingsVariables,
};
use warp_graphql::mutations::populate_merkle_tree_cache::{
    PopulateMerkleTreeCache, PopulateMerkleTreeCacheResult, PopulateMerkleTreeCacheVariables,
};
use warp_graphql::mutations::update_merkle_tree::{
    MerkleTreeNode, UpdateMerkleTree, UpdateMerkleTreeInput, UpdateMerkleTreeResult,
    UpdateMerkleTreeVariables,
};
use warp_graphql::queries::codebase_context_config::{
    CodebaseContextConfigQuery, CodebaseContextConfigResult, CodebaseContextConfigVariables,
};
use warp_graphql::queries::get_relevant_fragments::{
    GetRelevantFragmentsQuery, GetRelevantFragmentsResult, GetRelevantFragmentsVariables,
};
use warp_graphql::queries::rerank_fragments::{
    FragmentLocationInput, RerankFragmentInput, RerankFragments, RerankFragmentsResult,
    RerankFragmentsVariables,
};
use warp_graphql::queries::sync_merkle_tree::{
    SyncMerkleTree, SyncMerkleTreeInput, SyncMerkleTreeResult, SyncMerkleTreeVariables,
};
use warp_graphql::request_context::{ClientContext, OsContext, RequestContext};

use super::*;

const CONFIG: EmbeddingConfig = EmbeddingConfig::VoyageCode3512;

fn request_context() -> RequestContext {
    RequestContext {
        client_context: ClientContext { version: None },
        os_context: OsContext {
            category: None,
            linux_kernel_version: None,
            name: None,
            version: None,
        },
    }
}

fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Sends a client-built operation through [`CodebaseIndex::respond`] and parses the answer with
/// the client's type.
async fn call<Q, V>(index: &Arc<CodebaseIndex>, operation: cynic::Operation<Q, V>) -> Q
where
    Q: DeserializeOwned,
    V: serde::Serialize,
{
    let name = operation.operation_name.clone().unwrap().into_owned();
    let request = serde_json::to_value(&operation).unwrap();
    let response = index
        .respond(
            IndexOperation::from_name(&name).unwrap(),
            "user".to_owned(),
            request["variables"].clone(),
        )
        .await;
    let response: cynic::GraphQlResponse<Q> = serde_json::from_value(response.clone())
        .unwrap_or_else(|error| panic!("{error}: {response}"));
    response
        .data
        .unwrap_or_else(|| panic!("no data for {name}"))
}

fn index(dir: &tempfile::TempDir) -> Arc<CodebaseIndex> {
    Arc::new(CodebaseIndex::with_zvec_grep_binary(
        dir.path().to_owned(),
        None,
    ))
}

struct Repo {
    root: String,
    fragments: Vec<(String, String)>,
}

/// A one-file repository: the root node's only child is the file node holding `contents`.
fn repo(contents: &[&str]) -> (Repo, Vec<(String, Vec<String>)>) {
    let fragments: Vec<(String, String)> = contents
        .iter()
        .map(|content| (sha(content), (*content).to_owned()))
        .collect();
    let file = sha(&format!("file{contents:?}"));
    let root = sha(&format!("root{contents:?}"));
    let nodes = vec![
        (
            file.clone(),
            fragments.iter().map(|(hash, _)| hash.clone()).collect(),
        ),
        (root.clone(), vec![file]),
    ];
    (Repo { root, fragments }, nodes)
}

async fn sync(index: &Arc<CodebaseIndex>, hashes: Vec<String>) -> Vec<String> {
    let response = call(
        index,
        SyncMerkleTree::build(SyncMerkleTreeVariables {
            request_context: request_context(),
            input: SyncMerkleTreeInput {
                hashed_nodes: hashes.into_iter().map(NodeHash).collect(),
                embedding_config: CONFIG,
            },
        }),
    )
    .await;
    let SyncMerkleTreeResult::SyncMerkleTreeOutput(output) = response.sync_merkle_tree else {
        panic!("unexpected sync result");
    };
    output
        .changed_nodes
        .into_iter()
        .map(|hash| hash.0)
        .collect()
}

async fn upload(index: &Arc<CodebaseIndex>, repo: &Repo, nodes: Vec<(String, Vec<String>)>) {
    let response = call(
        index,
        GenerateCodeEmbeddings::build(GenerateCodeEmbeddingsVariables {
            input: GenerateCodeEmbeddingsInput {
                embedding_config: CONFIG,
                repo_metadata: RepoMetadata { path: None },
                fragments: repo
                    .fragments
                    .iter()
                    .map(|(hash, content)| Fragment {
                        content: content.clone(),
                        content_hash: ContentHash(hash.clone()),
                    })
                    .collect(),
                root_hash: NodeHash(repo.root.clone()),
            },
            request_context: request_context(),
        }),
    )
    .await;
    let GenerateCodeEmbeddingsResult::GenerateCodeEmbeddingsOutput(output) =
        response.generate_code_embeddings
    else {
        panic!("unexpected embeddings result");
    };
    assert!(output.embedding_results.iter().all(|result| result.success));

    let response = call(
        index,
        UpdateMerkleTree::build(UpdateMerkleTreeVariables {
            input: UpdateMerkleTreeInput {
                embedding_config: CONFIG,
                nodes: nodes
                    .into_iter()
                    .map(|(hash, children)| MerkleTreeNode {
                        hash: NodeHash(hash),
                        children: children.into_iter().map(NodeHash).collect(),
                    })
                    .collect(),
            },
            request_context: request_context(),
        }),
    )
    .await;
    let UpdateMerkleTreeResult::UpdateMerkleTreeOutput(output) = response.update_merkle_tree else {
        panic!("unexpected update result");
    };
    assert!(output.results.iter().all(|result| result.success));
}

async fn relevant(index: &Arc<CodebaseIndex>, root: &str, query: &str) -> Vec<String> {
    let response = call(
        index,
        GetRelevantFragmentsQuery::build(GetRelevantFragmentsVariables {
            embedding_config: CONFIG,
            repo_metadata: RepoMetadata { path: None },
            query: query.to_owned(),
            request_context: request_context(),
            root_hash: NodeHash(root.to_owned()),
        }),
    )
    .await;
    let GetRelevantFragmentsResult::GetRelevantFragmentsOutput(output) =
        response.get_relevant_fragments
    else {
        panic!("unexpected relevant fragments result");
    };
    output
        .candidate_hashes
        .into_iter()
        .map(|hash| hash.0)
        .collect()
}

#[tokio::test]
async fn config_names_an_embedding_config_and_cadence() {
    let dir = tempfile::tempdir().unwrap();
    let response = call(
        &index(&dir),
        CodebaseContextConfigQuery::build(CodebaseContextConfigVariables {
            request_context: request_context(),
        }),
    )
    .await;
    let CodebaseContextConfigResult::CodebaseContextConfigOutput(output) =
        response.codebase_context_config
    else {
        panic!("unexpected config result");
    };
    assert_eq!(output.embedding_cadence, EMBEDDING_CADENCE_SECS);
    assert!(matches!(
        output.embedding_config,
        EmbeddingConfig::VoyageCode3512
    ));
}

#[tokio::test]
async fn a_synced_repository_answers_queries_from_its_own_fragments() {
    let dir = tempfile::tempdir().unwrap();
    let index = index(&dir);
    let (repo_a, nodes_a) = repo(&[
        "fn render_tab_bar(tabs: &[Tab]) {}",
        "fn load_workspace_config(path: &Path) -> WorkspaceConfig {}",
    ]);
    let (repo_b, nodes_b) = repo(&["fn load_workspace_config_v2() {}"]);

    assert_eq!(
        sync(&index, vec![repo_a.root.clone()]).await,
        std::slice::from_ref(&repo_a.root)
    );
    upload(&index, &repo_a, nodes_a).await;
    upload(&index, &repo_b, nodes_b).await;
    assert!(sync(&index, vec![repo_a.root.clone()]).await.is_empty());

    let response = call(
        &index,
        PopulateMerkleTreeCache::build(PopulateMerkleTreeCacheVariables {
            embedding_config: CONFIG,
            root_hash: NodeHash(repo_a.root.clone()),
            repo_metadata: RepoMetadata { path: None },
            request_context: request_context(),
        }),
    )
    .await;
    assert!(matches!(
        response.populate_merkle_tree_cache,
        PopulateMerkleTreeCacheResult::PopulateMerkleTreeCacheOutput(output) if output.success
    ));

    assert_eq!(
        relevant(&index, &repo_a.root, "workspace config loading").await,
        [repo_a.fragments[1].0.clone()]
    );

    let restarted = Arc::new(CodebaseIndex::with_zvec_grep_binary(
        dir.path().to_owned(),
        None,
    ));
    assert!(sync(&restarted, vec![repo_a.root.clone()]).await.is_empty());
    assert_eq!(
        relevant(&restarted, &repo_b.root, "workspace config").await,
        [repo_b.fragments[0].0.clone()]
    );
}

#[tokio::test]
async fn nodes_with_unknown_children_are_not_stored() {
    let dir = tempfile::tempdir().unwrap();
    let index = index(&dir);
    let response = call(
        &index,
        UpdateMerkleTree::build(UpdateMerkleTreeVariables {
            input: UpdateMerkleTreeInput {
                embedding_config: CONFIG,
                nodes: vec![MerkleTreeNode {
                    hash: NodeHash(sha("node")),
                    children: vec![NodeHash(sha("missing"))],
                }],
            },
            request_context: request_context(),
        }),
    )
    .await;
    let UpdateMerkleTreeResult::UpdateMerkleTreeOutput(output) = response.update_merkle_tree else {
        panic!("unexpected update result");
    };
    assert!(!output.results[0].success);
    assert_eq!(sync(&index, vec![sha("node")]).await, [sha("node")]);
}

#[tokio::test]
async fn reranking_puts_matches_first_and_keeps_every_fragment() {
    let dir = tempfile::tempdir().unwrap();
    let fragment = |content: &str, path: &str| RerankFragmentInput {
        content: content.to_owned(),
        content_hash: ContentHash(sha(content)),
        location: FragmentLocationInput {
            byte_end: 10,
            byte_start: 0,
            file_path: path.to_owned(),
        },
    };
    let response = call(
        &index(&dir),
        RerankFragments::build(RerankFragmentsVariables {
            fragments: vec![
                fragment("fn unrelated() {}", "src/a.rs"),
                fragment("fn draw() {}", "src/theme/colors.rs"),
            ],
            query: "theme colors".to_owned(),
            request_context: request_context(),
        }),
    )
    .await;
    let RerankFragmentsResult::RerankFragmentsOutput(output) = response.rerank_fragments else {
        panic!("unexpected rerank result");
    };
    let paths: Vec<_> = output
        .ranked_fragments
        .iter()
        .map(|fragment| fragment.location.file_path.as_str())
        .collect();
    assert_eq!(paths, ["src/theme/colors.rs", "src/a.rs"]);
}

#[cfg(unix)]
#[tokio::test]
async fn zvec_grep_answers_when_installed() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let (repo, nodes) = repo(&["fn alpha() {}", "fn beta() {}"]);
    let beta = repo.fragments[1].0.clone();
    let bin = tempfile::tempdir().unwrap();
    let script = bin.path().join("zg");
    // Returns `beta` for any query, which the built-in ranker would not for "alpha".
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ \"$1\" = --hybrid ] && echo \"{}/{beta}.txt\"\nexit 0\n",
            "x"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let index = Arc::new(CodebaseIndex::with_zvec_grep_binary(
        dir.path().to_owned(),
        Some(script.into_os_string()),
    ));
    upload(&index, &repo, nodes).await;
    assert_eq!(relevant(&index, &repo.root, "alpha").await, [beta]);
}

#[test]
fn relevant_files_match_paths_and_symbols() {
    let files = [
        ("src/tab_bar.rs".to_owned(), "render_tabs".to_owned()),
        (
            "src/settings/theme.rs".to_owned(),
            "ThemePicker apply_theme".to_owned(),
        ),
    ];
    assert_eq!(
        relevant_files("change the theme", &files),
        ["src/settings/theme.rs"]
    );
}
