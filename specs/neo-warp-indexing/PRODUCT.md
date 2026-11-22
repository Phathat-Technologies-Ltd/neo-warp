# Codebase Indexing Without Warp's Backend: Product Spec

## Problem

Warp's codebase indexing (agent codebase context, "relevant files") syncs a Merkle tree of the
user's repository to Warp's server, which embeds fragments and answers retrieval queries. neo-warp
runs without Warp's backend, so in organization builds those calls fail and agents get no
codebase context.

## Goal

Organization builds answer the client's indexing calls from the embedded server, backed by
[zvec-grep](https://github.com/zvec-ai/zvec-grep), with the index stored on the user's machine.
The client's indexing code, protocol, and UI stay unchanged.

## Behavior

- With the `codebase_indexing` feature enabled, indexing a repository works as it does against
  Warp's server: the client syncs, the index reports ready, and agents receive relevant
  fragments and files for their queries.
- Nothing leaves the machine. Fragments, the Merkle tree, and zvec-grep's index live under the
  app's data directory (`<data dir>/codebase-index/`). Deleting that directory resets every
  index; the client rebuilds it on its next sync.
- Retrieval quality:
  - When the `zg` binary (zvec-grep) is installed (on `PATH`, or at `ZVEC_GREP_BIN`), the server
    uses zvec-grep's hybrid (full-text plus local-embedding) search. zvec-grep's default local
    embedding model runs on-device; no credentials are involved.
  - Without `zg`, the server falls back to a built-in lexical (BM25) ranker over the same stored
    fragments, so indexing still works with keyword-level relevance.
- Reranking and relevant-file selection use the built-in local ranker. They make no model calls,
  so they never spend the user's provider credits.
- Indexes are kept per signed-in user and per embedding configuration.
- With the feature disabled, the embedded server answers indexing operations with the same
  "not supported" error it uses for other missing operations, and the client hides indexing UI
  (owned by the feature-flag work, not this change).

## Non-goals

- Cloud or shared team indexes.
- LLM-based reranking. If added later, it must use the request's own provider credentials (the
  `agent::turn` provider plumbing).
- Changing the client's chunking, Merkle tree, or sync cadence.

## Admin notes

zvec-grep is optional. For semantic retrieval, install it on user machines
(`npm install -g @zvec/zvec-grep`) or point `ZVEC_GREP_BIN` at the `zg` binary.
