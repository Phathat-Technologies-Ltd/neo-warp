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
        .download(
            &reqwest::Client::new(),
            &AtomicBool::new(false),
            |progress| last = Some(progress),
        )
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
        .download(
            &reqwest::Client::new(),
            &AtomicBool::new(false),
            |progress| {
                first.get_or_insert(progress);
            },
        )
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

#[test]
fn fp16_encoder_spec_is_pinned_into_the_parakeet_dir() {
    let spec = parakeet_fp16_encoder();
    assert_eq!(spec.id, parakeet_tdt_0_6b_v3().id);
    let [file] = spec.files.as_slice() else {
        panic!("expected one file, got {:?}", spec.files);
    };
    #[cfg(feature = "parakeet")]
    assert_eq!(file.name, crate::tdt::FP16_ENCODER_FILE);
    assert_eq!(file.size, 1_238_960_470);
    assert_eq!(file.sha256.len(), 64);
    let revision = file
        .url
        .strip_prefix("https://huggingface.co/phathat/parakeet-tdt-0.6b-v3-fp16-coreml/resolve/")
        .and_then(|rest| rest.strip_suffix("/encoder-model.fp16.onnx"))
        .expect("a file URL in the fp16 repo");
    assert!(
        revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "pinned to a commit, got {revision}"
    );
}

#[test]
fn fp16_encoder_installs_beside_the_int8_model() {
    let root = tempfile::tempdir().unwrap();
    let parakeet = ModelStore::new(root.path(), parakeet_tdt_0_6b_v3());
    let fp16 = ModelStore::new(root.path(), parakeet_fp16_encoder());
    assert_eq!(fp16.model_dir(), parakeet.model_dir());
    std::fs::create_dir_all(fp16.model_dir()).unwrap();
    std::fs::write(fp16.model_dir().join(VERIFIED_MARKER), b"").unwrap();
    let file = &fp16.spec().files[0];
    let encoder = std::fs::File::create(fp16.model_dir().join(&file.name)).unwrap();
    encoder.set_len(file.size - 1).unwrap();
    assert_eq!(
        fp16.status(),
        ModelStatus::Partial {
            downloaded: file.size - 1
        }
    );
    encoder.set_len(file.size).unwrap();
    assert_eq!(fp16.status(), ModelStatus::Installed);
    assert_eq!(parakeet.status(), ModelStatus::NotInstalled);
}

#[tokio::test]
async fn progress_counts_bytes_across_files_against_the_total() {
    let url = serve(Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let mut spec = spec(&url, sha(BODY));
    let mut second = spec.files[0].clone();
    second.name = "second.bin".to_owned();
    spec.files.push(second);
    let store = ModelStore::new(root.path(), spec);
    std::fs::create_dir_all(store.model_dir()).unwrap();
    std::fs::write(store.model_dir().join("weights.bin.part"), &BODY[..8]).unwrap();

    let mut reports = Vec::new();
    store
        .download(
            &reqwest::Client::new(),
            &AtomicBool::new(false),
            |progress| reports.push(progress),
        )
        .await
        .unwrap();
    assert!(reports.iter().all(|progress| progress.total == 40));
    assert_eq!(reports.first().map(|progress| progress.downloaded), Some(8));
    assert!(
        reports
            .windows(2)
            .all(|pair| pair[0].downloaded <= pair[1].downloaded)
    );
    // The second file's bytes add to the first file's rather than restarting from zero.
    assert!(reports.iter().any(|progress| progress.downloaded > 20));
    assert_eq!(reports.last().map(|progress| progress.downloaded), Some(40));
}

#[test]
fn percent_is_whole_and_unknown_without_a_total() {
    let progress = |downloaded, total| DownloadProgress { downloaded, total };
    assert_eq!(progress(0, 0).percent(), None);
    assert_eq!(progress(0, 670).percent(), Some(0));
    assert_eq!(progress(550_000_000, 670_479_942).percent(), Some(82));
    assert_eq!(progress(670, 670).percent(), Some(100));
}

#[test]
fn throttle_reports_on_percent_change_interval_and_completion() {
    let progress = |downloaded| DownloadProgress {
        downloaded,
        total: 1000,
    };
    let start = Instant::now();
    let at = |millis| start + Duration::from_millis(millis);
    let mut throttle = ProgressThrottle::new(Duration::from_millis(500));
    assert!(throttle.should_report(progress(0), at(0)));
    assert!(!throttle.should_report(progress(5), at(10)));
    assert!(throttle.should_report(progress(10), at(20)));
    assert!(!throttle.should_report(progress(15), at(100)));
    assert!(throttle.should_report(progress(16), at(520)));
    assert!(throttle.should_report(progress(1000), at(521)));
}

#[tokio::test]
async fn bundle_downloads_every_model_against_one_total() {
    let url = serve(Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let mut second = spec(&url, sha(BODY));
    second.id = "second-model".to_owned();
    second.display_name = "Second".to_owned();
    let bundle = ModelBundle::new(vec![
        ModelStore::new(root.path(), spec(&url, sha(BODY))),
        ModelStore::new(root.path(), second),
    ]);
    assert_eq!(bundle.display_name(), "Test and Second");
    assert_eq!(bundle.status(), ModelStatus::NotInstalled);
    let mut reports = Vec::new();
    bundle
        .download(
            &reqwest::Client::new(),
            &AtomicBool::new(false),
            |progress| reports.push(progress),
        )
        .await
        .unwrap();
    assert!(bundle.is_installed());
    assert!(reports.iter().all(|progress| progress.total == 40));
    assert_eq!(reports.last().map(|progress| progress.downloaded), Some(40));
    bundle.stores()[1].remove().unwrap();
    assert_eq!(bundle.status(), ModelStatus::Partial { downloaded: 20 });
    bundle.remove().unwrap();
    assert_eq!(bundle.status(), ModelStatus::NotInstalled);
}

#[test]
fn silero_spec_is_pinned() {
    let spec = silero_vad_v5();
    assert_eq!(spec.files.len(), 1);
    assert!(spec.files[0].url.contains("/v5.1.2/"));
    assert_eq!(spec.files[0].sha256.len(), 64);
}
