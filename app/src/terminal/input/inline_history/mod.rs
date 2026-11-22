//! Inline history menu for up-arrow history when `FeatureFlag::AgentView` is enabled.
//!
//! Shows both live conversations for the terminal view and command history in the terminal
//! view, and prompts and command history in the agent view.
mod data_source;
mod search_item;
mod view;

pub use data_source::{AcceptHistoryItem, InlineHistoryMenuDataSource};
/// Cycles the history scope while the history menu is open.
pub const HISTORY_SCOPE_CYCLE_KEYSTROKE: &str = "ctrl-s";
/// Cycles the history source (all, me, agent) while the history menu is open.
pub const HISTORY_SOURCE_CYCLE_KEYSTROKE: &str = "ctrl-o";

pub use view::{HistoryTab, InlineHistoryMenuEvent, InlineHistoryMenuView};
