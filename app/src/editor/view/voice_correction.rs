use local_transcription::bias::BiasTerm;
use local_transcription::context_correction::{Correction, CorrectionContext};
use string_offset::ByteOffset;
use warp_core::ui::theme::AnsiColorIdentifier;
use warpui::{SingletonEntity, ViewContext};

use super::EditorView;
use crate::ai::voice::transcribe::VoiceBiasTerm;
use crate::appearance::Appearance;
use crate::editor::TextStyleOperation;
use crate::view_components::{DismissibleToast, ToastLink};
use crate::voice::context_correction::{
    self, SharedVocabulary, VoiceCorrections, VoiceCorrectionsEvent,
};
use crate::workspace::{ToastStack, WorkspaceAction};

impl EditorView {
    /// Sets the vocabulary that voice transcripts inserted into this editor are corrected
    /// towards, or `None` to stop correcting.
    pub fn set_voice_vocabulary(
        &mut self,
        vocabulary: Option<SharedVocabulary>,
        ctx: &mut ViewContext<Self>,
    ) {
        if vocabulary.is_some()
            && !self.voice_corrections_subscribed
            && ctx.has_singleton_model::<VoiceCorrections>()
        {
            self.voice_corrections_subscribed = true;
            ctx.subscribe_to_model(&VoiceCorrections::handle(ctx), |me, _, event, ctx| {
                if let VoiceCorrectionsEvent::Revert(pending) = event {
                    me.revert_voice_correction(&pending.heard, &pending.corrected, ctx);
                }
            });
        }
        self.voice_vocabulary = vocabulary;
    }

    /// Sets the terms local transcription of voice input into this editor is biased toward.
    pub fn set_voice_bias_terms(&mut self, terms: Vec<BiasTerm>) {
        self.voice_bias_terms = terms;
    }

    /// The bias terms to send with a transcription request: only to local transcription, so
    /// screen contents never reach a hosted provider.
    pub(super) fn voice_bias_terms_for_request(&self) -> Vec<VoiceBiasTerm> {
        if !crate::voice::is_voice_transcribed_locally() {
            return Vec::new();
        }
        self.voice_bias_terms
            .iter()
            .map(|term| VoiceBiasTerm {
                text: term.text.clone(),
                trusted: term.trusted,
            })
            .collect()
    }

    /// Corrects a voice transcript towards this editor's vocabulary. Returns the text unchanged
    /// when correction is off or there is no vocabulary yet.
    pub fn correct_voice_transcript(
        &mut self,
        text: &str,
        context: CorrectionContext,
        ctx: &mut ViewContext<Self>,
    ) -> (String, Vec<(Correction, u64)>) {
        let Some(vocabulary) = self.voice_vocabulary.clone() else {
            return (text.to_owned(), Vec::new());
        };
        if !context_correction::is_enabled(ctx) || !ctx.has_singleton_model::<VoiceCorrections>() {
            return (text.to_owned(), Vec::new());
        }
        VoiceCorrections::handle(ctx).update(ctx, |corrections, ctx| {
            corrections.correct(text, &vocabulary, context, ctx)
        })
    }

    /// Underlines corrections in text that starts at byte `text_start` of the buffer, and offers
    /// to revert them.
    pub fn show_voice_corrections(
        &mut self,
        text_start: usize,
        corrections: &[(Correction, u64)],
        ctx: &mut ViewContext<Self>,
    ) {
        if corrections.is_empty() {
            return;
        }
        let ranges: Vec<_> = corrections
            .iter()
            .map(|(c, _)| {
                ByteOffset::from(text_start + c.range.start)
                    ..ByteOffset::from(text_start + c.range.end)
            })
            .collect();
        let color = AnsiColorIdentifier::Blue
            .to_ansi_color(&Appearance::as_ref(ctx).theme().terminal_colors().normal)
            .into();
        self.update_buffer_styles(
            ranges,
            TextStyleOperation::default().set_error_underline_color(color),
            ctx,
        );
        let Some(message) = context_correction::corrections_toast_text(corrections) else {
            return;
        };
        let window_id = ctx.window_id();
        let ids = corrections.iter().map(|(_, id)| *id).collect();
        let toast = DismissibleToast::default(message).with_link(
            ToastLink::new("Revert".to_owned())
                .with_onclick_action(WorkspaceAction::RevertVoiceCorrection(ids)),
        );
        ToastStack::handle(ctx).update(ctx, |toasts, ctx| {
            toasts.add_ephemeral_toast(toast, window_id, ctx);
        });
    }

    /// Restores the heard words in place of the last occurrence of `corrected`.
    fn revert_voice_correction(
        &mut self,
        heard: &str,
        corrected: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        let buffer = self.buffer_text(ctx);
        let Some(start) = buffer.rfind(corrected) else {
            return;
        };
        let reverted = format!(
            "{}{heard}{}",
            &buffer[..start],
            &buffer[start + corrected.len()..]
        );
        self.set_buffer_text(&reverted, ctx);
    }
}
