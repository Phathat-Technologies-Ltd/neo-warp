use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use axum::Router;
use axum::http::{HeaderMap, StatusCode as AxumStatus, header as axum_header};
use axum::response::IntoResponse;
use axum::routing::get;

use super::*;

const BODY: &[u8] = b"0123456789abcdefghij";

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Serves `BODY` at `/file`, honoring `Range: bytes=N-`, and counts requests.
async fn serve(hits: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/file",
        get(move |headers: HeaderMap| {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let start = headers
                    .get(axum_header::RANGE)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("bytes="))
                    .and_then(|value| value.trim_end_matches('-').parse::<usize>().ok());
                match start {
                    Some(start) => (AxumStatus::PARTIAL_CONTENT, BODY[start..].to_vec()),
                    None => (AxumStatus::OK, BODY.to_vec()),
                }
                .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}/file")
}

fn spec(url: &str, sha256: String) -> ModelSpec {
    ModelSpec {
        id: "test-model".to_owned(),
        display_name: "Test".to_owned(),
        files: vec![ModelFile {
            name: "weights.bin".to_owned(),
            url: url.to_owned(),
            size: BODY.len() as u64,
            sha256,
        }],
    }
}

#[tokio::test]
async fn downloads_verifies_and_removes() {
    let url = serve(Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let store = ModelStore::new(root.path(), spec(&url, sha(BODY)));
    assert_eq!(store.status(), ModelStatus::NotInstalled);

    let mut last = None;
    store
        .download(&reqwest::Client::new(), &AtomicBool::new(false), |progress| {
            last = Some(progress)
        })
        .await
        .unwrap();
    assert_eq!(
        last,
        Some(DownloadProgress {
            downloaded: 20,
            total: 20
        })
    );
    assert!(store.is_installed());
    assert_eq!(
        std::fs::read(store.model_dir().join("weights.bin")).unwrap(),
        BODY
    );

    store.remove().unwrap();
    assert_eq!(store.status(), ModelStatus::NotInstalled);
    store.remove().unwrap();
}

#[tokio::test]
async fn resumes_a_partial_file() {
    let url = serve(Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let store = ModelStore::new(root.path(), spec(&url, sha(BODY)));
    std::fs::create_dir_all(store.model_dir()).unwrap();
    std::fs::write(store.model_dir().join("weights.bin.part"), &BODY[..8]).unwrap();
    assert_eq!(store.status(), ModelStatus::Partial { downloaded: 8 });

    let mut first = None;
    store
        .download(&reqwest::Client::new(), &AtomicBool::new(false), |progress| {
            first.get_or_insert(progress);
        })
        .await
        .unwrap();
    assert_eq!(first.map(|progress| progress.downloaded), Some(8));
    assert!(store.is_installed());
}

#[tokio::test]
async fn bad_checksum_deletes_the_file() {
    let url = serve(Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let store = ModelStore::new(root.path(), spec(&url, sha(b"other")));
    let result = store
        .download(&reqwest::Client::new(), &AtomicBool::new(false), |_| {})
        .await;
    assert!(matches!(result, Err(DownloadError::Checksum { .. })));
    assert_eq!(store.status(), ModelStatus::NotInstalled);
}

#[tokio::test]
async fn installed_model_is_not_downloaded_again() {
    let hits = Arc::new(AtomicUsize::new(0));
    let url = serve(hits.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let store = ModelStore::new(root.path(), spec(&url, sha(BODY)));
    let client = reqwest::Client::new();
    let cancel = AtomicBool::new(false);
    store.download(&client, &cancel, |_| {}).await.unwrap();
    store.download(&client, &cancel, |_| {}).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn parakeet_spec_is_pinned() {
    let spec = parakeet_tdt_0_6b_v3();
    assert_eq!(spec.files.len(), 3);
    assert!(spec.files.iter().all(|file| file.sha256.len() == 64));
    assert_eq!(spec.total_size(), 670_479_942);
}
