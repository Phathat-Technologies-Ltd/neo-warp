use chrono::{DateTime, Duration, Local};
use fuzzy_match::FuzzyMatchResult;
use ordered_float::OrderedFloat;
use warp_core::ui::Icon;
use warp_core::ui::color::coloru_with_opacity;
use warp_core::ui::theme::Fill;
use warpui::elements::{ConstrainedBox, Container, Expanded, Highlight, ParentElement, Text};
use warpui::fonts::{Properties, Weight};
use warpui::prelude::{Align, CrossAxisAlignment, Flex, MainAxisAlignment, MainAxisSize};
use warpui::scene::{CornerRadius, Radius};
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, Element, SingletonEntity};

use crate::ai::agent::conversation::{AIConversationId, ConversationStatus};
use crate::ai::conversation_status_ui::{STATUS_ELEMENT_PADDING, render_status_element};
use crate::appearance::Appearance;
use crate::search::{ItemHighlightState, SearchItem};
use crate::terminal::history::{HistoryEntry, LinkedWorkflowData};
use crate::terminal::input::inline_history::data_source::AcceptHistoryItem;
use crate::terminal::input::inline_menu::styles as inline_styles;
use crate::util::time_format::format_approx_duration_from_now_utc;

#[derive(Debug, Clone)]
pub struct InlineHistoryItem {
    item_type: HistoryItemType,
    name_match_result: Option<FuzzyMatchResult>,
    prefix_match_len: usize,
    score: OrderedFloat<f64>,
    timestamp: DateTime<Local>,
    run: Option<CommandRun>,
}

/// How a finished history command ran. Entries without full block metadata (e.g. ones read from
/// the shell's history file) have none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRun {
    pub duration: Duration,
    pub succeeded: bool,
}

impl CommandRun {
    pub fn from_entry(entry: &HistoryEntry) -> Option<Self> {
        let duration = entry.completed_ts? - entry.start_ts?;
        Some(Self {
            duration: duration.max(Duration::zero()),
            succeeded: entry.exit_code?.value() == 0,
        })
    }
}

/// Compact duration such as `120ms`, `4s`, `2m 3s`, or `1h 5m`.
pub fn format_command_duration(duration: Duration) -> String {
    let millis = duration.num_milliseconds().max(0);
    if millis < 1000 {
        return format!("{millis}ms");
    }
    let secs = millis / 1000;
    if secs < 60 {
        return format!("{secs}s");
    }
    let (mins, secs) = (secs / 60, secs % 60);
    if mins < 60 {
        return format!("{mins}m {secs}s");
    }
    format!("{}h {}m", mins / 60, mins % 60)
}

/// Width of the duration column, in monospace ems. Fits the longest compact duration (`59m 59s`).
const DURATION_COLUMN_EMS: f32 = 7.;
/// Width of the relative time column, in UI font ems.
const TIME_COLUMN_EMS: f32 = 8.;
const COLUMN_GAP: f32 = 8.;

/// The columns of a history row: an optional fixed-width, right-aligned duration, the text
/// left-aligned and filling the remaining width, and a fixed-width, right-aligned relative time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRowLayout {
    /// Present for every command row so command text lines up, even when the run is unknown.
    pub duration: Option<DurationColumn>,
    pub relative_time: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurationColumn {
    /// Empty when the run is unknown.
    pub label: String,
    pub succeeded: Option<bool>,
}

#[derive(Debug, Clone)]
enum HistoryItemType {
    Conversation {
        conversation_id: AIConversationId,
        title: String,
        status: ConversationStatus,
    },
    Command {
        command: String,
        linked_workflow_data: Option<LinkedWorkflowData>,
    },
    AIPrompt {
        query_text: String,
    },
}

impl InlineHistoryItem {
    pub fn conversation(
        conversation_id: AIConversationId,
        title: String,
        status: ConversationStatus,
        timestamp: DateTime<Local>,
    ) -> Self {
        Self {
            item_type: HistoryItemType::Conversation {
                conversation_id,
                title,
                status,
            },
            name_match_result: None,
            prefix_match_len: 0,
            score: OrderedFloat(f64::MIN),
            timestamp,
            run: None,
        }
    }

    pub fn command(
        command: String,
        linked_workflow_data: Option<LinkedWorkflowData>,
        timestamp: DateTime<Local>,
    ) -> Self {
        Self {
            item_type: HistoryItemType::Command {
                command,
                linked_workflow_data,
            },
            name_match_result: None,
            prefix_match_len: 0,
            score: OrderedFloat(f64::MIN),
            timestamp,
            run: None,
        }
    }

    pub fn ai_prompt(query_text: String, timestamp: DateTime<Local>) -> Self {
        Self {
            item_type: HistoryItemType::AIPrompt { query_text },
            name_match_result: None,
            prefix_match_len: 0,
            score: OrderedFloat(f64::MIN),
            timestamp,
            run: None,
        }
    }

    pub fn with_run(mut self, run: Option<CommandRun>) -> Self {
        self.run = run;
        self
    }

    pub fn with_name_match_result(mut self, result: Option<FuzzyMatchResult>) -> Self {
        self.name_match_result = result;
        self
    }

    pub fn with_prefix_match_len(mut self, len: usize) -> Self {
        self.prefix_match_len = len;
        self
    }

    pub fn with_score(mut self, score: OrderedFloat<f64>) -> Self {
        self.score = score;
        self
    }
}

impl InlineHistoryItem {
    pub fn row_layout(&self) -> HistoryRowLayout {
        let duration = match &self.item_type {
            HistoryItemType::Command { .. } => Some(match self.run {
                Some(run) => DurationColumn {
                    label: format_command_duration(run.duration),
                    succeeded: Some(run.succeeded),
                },
                None => DurationColumn {
                    label: String::new(),
                    succeeded: None,
                },
            }),
            HistoryItemType::Conversation { .. } | HistoryItemType::AIPrompt { .. } => None,
        };
        HistoryRowLayout {
            duration,
            relative_time: format_approx_duration_from_now_utc(self.timestamp.to_utc()),
        }
    }
}

impl SearchItem for InlineHistoryItem {
    type Action = AcceptHistoryItem;

    fn render_icon(
        &self,
        _highlight_state: ItemHighlightState,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let icon_size = inline_styles::font_size(appearance);
        let icon = match &self.item_type {
            HistoryItemType::Conversation { status, .. } => {
                render_status_element(status, icon_size, appearance)
            }
            HistoryItemType::Command { .. } => {
                let icon_color = inline_styles::icon_color(appearance);
                Container::new(
                    ConstrainedBox::new(Icon::Terminal.to_warpui_icon(icon_color).finish())
                        .with_width(icon_size)
                        .with_height(icon_size)
                        .finish(),
                )
                .with_uniform_padding(STATUS_ELEMENT_PADDING)
                .with_background(coloru_with_opacity(icon_color.into(), 10))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
                    inline_styles::ITEM_CORNER_RADIUS,
                )))
                .finish()
            }
            HistoryItemType::AIPrompt { .. } => {
                let icon_color = inline_styles::icon_color(appearance);
                Container::new(
                    ConstrainedBox::new(Icon::Prompt.to_warpui_icon(icon_color).finish())
                        .with_width(icon_size)
                        .with_height(icon_size)
                        .finish(),
                )
                .with_uniform_padding(STATUS_ELEMENT_PADDING)
                .with_background(coloru_with_opacity(icon_color.into(), 10))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
                    inline_styles::ITEM_CORNER_RADIUS,
                )))
                .finish()
            }
        };

        Container::new(icon)
            .with_margin_right(inline_styles::ICON_MARGIN)
            .finish()
    }

    fn render_item(
        &self,
        _highlight_state: ItemHighlightState,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::handle(app).as_ref(app);
        let theme = appearance.theme();
        let font_size = inline_styles::font_size(appearance);
        let background_color = inline_styles::menu_background_color(app);

        let primary_text_color = inline_styles::primary_text_color(theme, background_color.into());
        let secondary_text_color =
            inline_styles::secondary_text_color(theme, background_color.into());

        let (display_text, match_indices, font_family) = match &self.item_type {
            HistoryItemType::Conversation { title, .. } => {
                let indices = self
                    .name_match_result
                    .as_ref()
                    .map(|m| m.matched_indices.clone())
                    .unwrap_or_default();
                (title.clone(), indices, appearance.ui_font_family())
            }
            HistoryItemType::Command { command, .. } => {
                let indices = if self.prefix_match_len > 0 {
                    (0..self.prefix_match_len).collect()
                } else {
                    vec![]
                };
                (command.clone(), indices, appearance.monospace_font_family())
            }
            HistoryItemType::AIPrompt { query_text } => {
                let indices = if self.prefix_match_len > 0 {
                    (0..self.prefix_match_len).collect()
                } else {
                    vec![]
                };
                (query_text.clone(), indices, appearance.ui_font_family())
            }
        };

        let mut text = Text::new_inline(display_text, font_family, font_size)
            .with_color(primary_text_color.into())
            .with_clip(ClipConfig::ellipsis());

        if !match_indices.is_empty() {
            text = text.with_single_highlight(
                Highlight::new().with_properties(Properties::default().weight(Weight::Bold)),
                match_indices,
            );
        }

        let layout = self.row_layout();
        let em_width = |family| app.font_cache().em_width(family, font_size);
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::Start)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);

        if let Some(duration) = layout.duration {
            let theme = appearance.theme();
            let color = match duration.succeeded {
                Some(true) => theme.ansi_fg_green(),
                Some(false) => theme.ansi_fg_red(),
                None => secondary_text_color.into_solid(),
            };
            let label = Text::new_inline(
                duration.label,
                appearance.monospace_font_family(),
                font_size,
            )
            .with_color(color)
            .finish();
            row.add_child(
                Container::new(
                    ConstrainedBox::new(Align::new(label).right().finish())
                        .with_width(
                            em_width(appearance.monospace_font_family()) * DURATION_COLUMN_EMS,
                        )
                        .finish(),
                )
                .with_margin_right(COLUMN_GAP)
                .finish(),
            );
        }

        row.add_child(Expanded::new(1., Align::new(text.finish()).left().finish()).finish());

        let timestamp =
            Text::new_inline(layout.relative_time, appearance.ui_font_family(), font_size)
                .with_color(secondary_text_color.into())
                .finish();
        row.add_child(
            Container::new(
                ConstrainedBox::new(Align::new(timestamp).right().finish())
                    .with_width(em_width(appearance.ui_font_family()) * TIME_COLUMN_EMS)
                    .finish(),
            )
            .with_margin_left(COLUMN_GAP)
            .finish(),
        );

        row.finish()
    }

    fn item_background(
        &self,
        highlight_state: ItemHighlightState,
        appearance: &Appearance,
    ) -> Option<Fill> {
        inline_styles::item_background(highlight_state, appearance)
    }

    fn score(&self) -> OrderedFloat<f64> {
        self.score
    }

    fn accept_result(&self) -> Self::Action {
        match &self.item_type {
            HistoryItemType::Conversation {
                conversation_id,
                title,
                ..
            } => AcceptHistoryItem::Conversation {
                conversation_id: *conversation_id,
                title: title.clone(),
            },
            HistoryItemType::Command {
                command,
                linked_workflow_data,
            } => AcceptHistoryItem::Command {
                command: command.clone(),
                linked_workflow_data: linked_workflow_data.clone(),
            },
            HistoryItemType::AIPrompt { query_text } => AcceptHistoryItem::AIPrompt {
                query_text: query_text.clone(),
            },
        }
    }

    fn execute_result(&self) -> Self::Action {
        self.accept_result()
    }

    fn accessibility_label(&self) -> String {
        match &self.item_type {
            HistoryItemType::Conversation { title, .. } => format!("Conversation: {title}"),
            HistoryItemType::Command { command, .. } => format!("Command: {command}"),
            HistoryItemType::AIPrompt { query_text } => format!("AI prompt: {query_text}"),
        }
    }
}

#[cfg(test)]
#[path = "search_item_tests.rs"]
mod tests;
