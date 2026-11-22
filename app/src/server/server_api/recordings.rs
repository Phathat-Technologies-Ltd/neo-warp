use crate::ChannelState;
use anyhow::Context as _;
use async_trait::async_trait;
use session_recording::{CreateRecording, CreatedRecording, RecordingMetadata, RenameRecording};

use super::ServerApi;
use crate::terminal::session_recording::client::RecordingsClient;

fn recordings_url(path: &str) -> String {
    format!("{}/recordings{path}", ChannelState::server_root_url())
}

fn id_path(id: &str) -> String {
    format!("/{}", urlencoding::encode(id))
}

impl ServerApi {
    async fn recordings_request(
        &self,
        build: impl FnOnce(&http_client::Client) -> http_client::RequestBuilder<'_>,
    ) -> anyhow::Result<http_client::Response> {
        let auth_token = self.get_or_refresh_access_token().await?;
        let mut request = build(self.base_client.http_client());
        if let Some(token) = auth_token.as_bearer_token() {
            request = request.bearer_auth(token);
        }
        Ok(request.send().await?.error_for_status_with_body().await?)
    }
}

#[cfg_attr(target_family = "wasm", async_trait(?Send))]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
impl RecordingsClient for ServerApi {
    async fn start(&self, request: &CreateRecording) -> anyhow::Result<String> {
        let created: CreatedRecording = self
            .recordings_request(|client| client.post(recordings_url("")).json(request))
            .await?
            .json()
            .await
            .context("invalid start-recording response")?;
        Ok(created.id)
    }

    async fn append(&self, id: &str, seq: u64, events: String) -> anyhow::Result<()> {
        let url = recordings_url(&format!("{}/events?seq={seq}", id_path(id)));
        self.recordings_request(|client| {
            client
                .post(url)
                .header("content-type", "text/plain; charset=utf-8")
                .body(events)
        })
        .await?;
        Ok(())
    }

    async fn finish(&self, id: &str) -> anyhow::Result<RecordingMetadata> {
        let url = recordings_url(&format!("{}/finish", id_path(id)));
        Ok(self
            .recordings_request(|client| client.post(url))
            .await?
            .json()
            .await?)
    }

    async fn list(&self) -> anyhow::Result<Vec<RecordingMetadata>> {
        Ok(self
            .recordings_request(|client| client.get(recordings_url("")))
            .await?
            .json()
            .await?)
    }

    async fn read(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        let url = recordings_url(&id_path(id));
        Ok(self
            .recordings_request(|client| client.get(url))
            .await?
            .bytes()
            .await?
            .to_vec())
    }

    async fn rename(&self, id: &str, name: &str) -> anyhow::Result<RecordingMetadata> {
        let url = recordings_url(&id_path(id));
        let body = RenameRecording {
            name: name.to_owned(),
        };
        Ok(self
            .recordings_request(|client| client.patch(url).json(&body))
            .await?
            .json()
            .await?)
    }

    async fn delete(&self, id: &str) -> anyhow::Result<()> {
        let url = recordings_url(&id_path(id));
        self.recordings_request(|client| client.delete(url)).await?;
        Ok(())
    }
}
