//! A test provider that answers each model call with a scripted text reply and records what the
//! call saw.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use oi_kit_provider_types::base::{MessageStream, Provider};
use oi_kit_provider_types::conversation::message::Message;
use oi_kit_provider_types::errors::ProviderError;
use oi_kit_provider_types::model::ModelConfig;
use rmcp::model::Tool;

use super::ProviderFactory;

/// One model call: the model config's `max_tokens`, the system prompt, and the user text.
pub(crate) type RecordedCall = (Option<i32>, String, String);

pub(crate) struct TextProvider {
    replies: Mutex<VecDeque<Result<String, String>>>,
    pub calls: Mutex<Vec<RecordedCall>>,
}

impl TextProvider {
    /// Replies in order; an `Err` fails the call with that provider error text.
    pub fn new(replies: Vec<Result<&str, &str>>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|reply| reply.map(str::to_owned).map_err(str::to_owned))
                    .collect(),
            ),
            calls: Mutex::default(),
        })
    }

    pub fn factory(self: &Arc<Self>) -> ProviderFactory {
        let provider = self.clone();
        Arc::new(move |_| Ok(provider.clone() as Arc<dyn Provider>))
    }

    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for TextProvider {
    fn get_name(&self) -> &str {
        "scripted"
    }

    async fn stream(
        &self,
        model_config: &ModelConfig,
        system: &str,
        messages: &[Message],
        _tools: &[Tool],
    ) -> Result<MessageStream, ProviderError> {
        self.calls.lock().unwrap().push((
            model_config.max_tokens,
            system.to_owned(),
            messages
                .iter()
                .map(Message::as_concat_text)
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(String::new()));
        match reply {
            Ok(text) => Ok(Box::pin(futures::stream::iter([Ok((
                Some(Message::assistant().with_text(text)),
                None,
            ))]))),
            Err(error) => Err(ProviderError::RequestFailed(error)),
        }
    }
}
