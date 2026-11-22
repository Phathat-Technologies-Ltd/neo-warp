# Codebase Indexing Without Warp's Backend: Tech Spec

See `PRODUCT.md` for scope.

## Client protocol (unchanged)

`crates/ai/src/index/full_source_code_embedding` builds a Merkle tree per repository. Leaves are
fragments identified by `ContentHash` (sha256, 64 lowercase hex characters); intermediate nodes
(`NodeHash`) hash their children, which may be leaves or other nodes. The `StoreClient` in
`app/src/server/server_api/ai.rs` calls these GraphQL operations (cynic types in
`crates/graphql/src/api`):

| Operation (`?op=`)           | Server meaning                                                 |
|------------------------------|----------------------------------------------------------------|
| `CodebaseContextConfigQuery` | Embedding config namespace and sync cadence (seconds).         |
| `SyncMerkleTree`             | Of `hashedNodes`, return those the server does not know.       |
| `UpdateMerkleTree`           | Store intermediate nodes; success when all children are known. |
| `GenerateCodeEmbeddings`     | Store fragment contents by content hash and index them.        |
| `PopulateMerkleTreeCache`    | Success when the root is known.                                |
| `GetRelevantFragmentsQuery`  | Top content hashes reachable from `rootHash` for `query`.      |
| `RerankFragments`            | Reorder the given fragments for `query`.                       |

Plus REST `POST /ai/relevant_files` (`app/src/ai/get_relevant_files/api.rs`): `{query, files:
[{path, symbols}]}` answered with `{relevant_file_paths}`.

The client sends fragment *contents* only, never paths, so the server-side index is a
content-addressed store, not a checkout.

## zvec-grep integration decision

zvec-grep is a Rust workspace (`rust/crates/zg-engine` exposes `ZvecGrep::index` and
`ZvecGrep::context`) distributed as the `zg` CLI. Embedding it as a library is not viable today:
its crates are `publish = false`, need Rust 1.98 (Warp pins 1.95), and pull native llama.cpp,
ONNX Runtime and zvec builds into the app. The CLI dropped JSON output, but it prints the
indexed files each hit came from.

So the adapter runs `zg` as a short-lived child process (no resident daemon):

- The store writes each fragment to `fragments/<content-hash>.txt`. That directory is a zvec-grep
  workspace; zvec-grep keeps its own index in `fragments/.zvec-grep/`.
- New fragments mark the workspace dirty; the next query runs `zg --index` there first.
- A query runs `zg --hybrid <query> --limit N --preview none --color never` in the workspace and
  collects every `<64 hex>.txt` name in stdout, in order. File names are content hashes, so the
  output maps back to `ContentHash`es without depending on the human output format.
- Hits are filtered to the fragments reachable from the requested root, since one workspace
  serves every repository in an embedding config.
- If `zg` is missing or fails, the built-in BM25 ranker answers instead.

When zg-engine becomes a publishable crate on our toolchain, the child process can be replaced by
an in-process `ZvecGrep::context` call without changing anything else.

## Server design (`crates/selfhost_server/src/indexing/`)

- `store.rs`: `IndexStore`, one directory per user (`sha256(uid)` prefix) and embedding config
  under the index root. `nodes.jsonl` appends `{hash, children}`; fragments are files. Loaded
  lazily into memory. Hashes are validated as 64 lowercase hex characters before they touch the
  disk.
- `lexical.rs`: tokenizer (splits identifiers on case and punctuation) and BM25 scoring.
- `zvec.rs`: the `zg` child-process runner and its output parser.
- `mod.rs`: `CodebaseIndex`, which answers each operation from its GraphQL variables, on
  `spawn_blocking`.
- `lib.rs`: the GraphQL handler reads the request body for variables and routes indexing
  operations to `CodebaseIndex`; `/ai/relevant_files` is a new route.
- `AppState::codebase_index: Option<Arc<CodebaseIndex>>`, set from
  `EmbeddedConfig::codebase_index_dir`. `None` keeps today's "not supported" answers. The app
  passes `data_dir()/codebase-index` (`app/src/auth/organization.rs`).

## Feature flag

Needs `FeatureFlag::CodebaseIndexing` (added by the flags work). At merge, the app should pass
`codebase_index_dir: None` when that flag is disabled, so the server stops answering; the
client's indexing UI and sync are hidden by the same flag.

## Testing

`crates/selfhost_server/src/indexing/*_tests.rs` and `lib_tests.rs`. Requests are built with the
client's cynic operations and responses deserialize through the same types. Covered: the full
sync round trip (unknown nodes, update, embeddings, populate, relevant fragments restricted to
the root), reranking, relevant files, persistence across a restart, hash validation, `zg` output
parsing, and a fake `zg` script driving the zvec-grep path.
