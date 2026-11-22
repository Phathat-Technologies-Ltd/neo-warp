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
  workspace; zvec-grep keeps its own index in `fragments/.zvec-grep/`. Fragments are first
  written to `incoming/<content-hash>.partial`, outside the workspace, and renamed into place; a
  failed write removes its partial file, and `incoming/` is cleared when a namespace is opened.
- A fragment is only stored if `sha256(content)` equals the content hash the client sent;
  mismatches are reported as a failed `GenerateCodeEmbeddings` result for that fragment.
- New fragments mark the workspace dirty; the next query runs `zg --index` there first.
- A query runs `zg --hybrid <query> --limit N --preview none --color never` in the workspace and
  collects every `<64 hex>.txt` name in stdout, in order. File names are content hashes, so the
  output maps back to `ContentHash`es without depending on the human output format.
- Hits are filtered to the fragments reachable from the requested root, since one workspace
  serves every repository in an embedding config.
- `zg --index` is killed after 120 seconds and `zg --hybrid` after 15 seconds, so a hung `zg`
  never ties up a `spawn_blocking` thread for long. stdout and stderr are drained on their own
  threads while the server waits.
- If `zg` is missing, fails, or times out, the built-in BM25 ranker answers instead. A failed
  `zg --index` leaves the workspace dirty, so the next query retries it.

When zg-engine becomes a publishable crate on our toolchain, the child process can be replaced by
an in-process `ZvecGrep::context` call without changing anything else.

## Server design (`crates/selfhost_server/src/indexing/`)

- `store.rs`: `IndexStore`, one directory per user (`sha256(uid)` prefix) and embedding config
  under the index root. `nodes.jsonl` appends `{hash, children}`; fragments are files. Loaded
  lazily into memory. Hashes are validated as 64 lowercase hex characters before they touch the
  disk.
- Locking: the store's `Mutex<IndexStore>` is held only to look up (or open) a namespace, which
  is shared as `Arc<NamespaceHandle>`. Each namespace has a `state` mutex for its nodes,
  fragments, and content cache, and a separate `zvec_grep` mutex held while `zg` runs over its
  workspace, so runs don't overlap but uploads and syncs proceed while `zg` runs. Users never
  wait on each other. Dirtiness is a generation counter: a reindex only marks the workspace clean
  up to the generation it started from, so fragments added during the run are indexed next time.
- The BM25 fallback reads fragment contents from a per-namespace in-memory cache (filled on
  upload and on read, capped at 64 MiB per namespace); uncached fragments are read from disk
  without holding the namespace lock, since fragment files never change once written.
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

Gated by `FeatureFlag::CodebaseIndexing`. The app passes `codebase_index_dir: None` when that
flag is off, which is all the server needs: `AppState::codebase_index` is then `None` and every
indexing operation gets today's "not supported" answer. The server has no flag check of its own.
The client's indexing UI and sync are hidden by the same flag.

## Testing

`crates/selfhost_server/src/indexing/*_tests.rs` and `lib_tests.rs`. Requests are built with the
client's cynic operations and responses deserialize through the same types. Covered: the full
sync round trip (unknown nodes, update, embeddings, populate, relevant fragments restricted to
the root), reranking, relevant files, persistence across a restart, hash validation (format and content
mismatch), partial writes staying out of the workspace, `zg` output parsing, killing a hung `zg`, and a fake `zg` script driving the zvec-grep path.
