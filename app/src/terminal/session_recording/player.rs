//! Replays an asciicast into a vt100 screen. Time is supplied by the caller so the playback logic
//! stays deterministic.

use super::asciicast::{Cast, EventData};
use super::naming::format_duration;

pub const SPEEDS: [f64; 4] = [0.5, 1., 2., 4.];
const SCROLLBACK_LINES: usize = 0;
/// Used when the recording's header has no `idle_time_limit`.
pub const DEFAULT_IDLE_TIME_LIMIT: f64 = 2.;
/// Going to the previous command within this many seconds of a command's start goes to the one
/// before it, like a media player's previous-track button.
const PREVIOUS_CHAPTER_GRACE: f64 = 1.;
/// A screen snapshot is kept after this many seconds of playback time or
/// [`SNAPSHOT_INTERVAL_EVENTS`] events since the previous one, so seeking replays a bounded number
/// of events.
const SNAPSHOT_INTERVAL_SECONDS: f64 = 5.;
const SNAPSHOT_INTERVAL_EVENTS: usize = 2000;

/// A command marker in the recording.
#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub label: String,
    /// Playback time of the marker, in seconds.
    pub time: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Playing,
    Paused,
    Ended,
}

pub struct Player {
    cast: Cast,
    /// Playback time of each event, with idle gaps capped when skipping idle.
    times: Vec<f64>,
    duration: f64,
    skip_idle: bool,
    screen: vt100::Parser,
    next_event: usize,
    /// Seconds into the recording.
    position: f64,
    speed: f64,
    state: PlaybackState,
    /// Screens after replaying the first `next_event` events, in increasing `next_event` order.
    /// They depend only on the events applied, so they stay valid when the timeline changes.
    snapshots: Vec<Snapshot>,
}

/// Taken between events, so it drops any escape sequence an event left unfinished. Recorders
/// write whole chunks of output, which keeps that rare.
struct Snapshot {
    next_event: usize,
    screen: vt100::Screen,
}

impl Player {
    /// A player paused at the start.
    pub fn new(cast: Cast) -> Self {
        let screen = new_screen(&cast);
        let mut player = Self {
            cast,
            times: Vec::new(),
            duration: 0.,
            skip_idle: true,
            screen,
            next_event: 0,
            position: 0.,
            speed: 1.,
            state: PlaybackState::Paused,
            snapshots: Vec::new(),
        };
        player.rebuild_timeline();
        // Shows the recording's opening frame before playback starts.
        player.apply_until(0.);
        player
    }

    pub fn idle_time_limit(&self) -> f64 {
        self.cast
            .header
            .idle_time_limit
            .filter(|limit| limit.is_finite() && *limit > 0.)
            .unwrap_or(DEFAULT_IDLE_TIME_LIMIT)
    }

    pub fn skip_idle(&self) -> bool {
        self.skip_idle
    }

    /// Turns idle capping on or off, keeping the same point in the recording.
    pub fn set_skip_idle(&mut self, skip_idle: bool) {
        if skip_idle == self.skip_idle {
            return;
        }
        let raw = self.recording_time_of(self.position);
        self.skip_idle = skip_idle;
        self.rebuild_timeline();
        let position = self.playback_time_of(raw).min(self.duration);
        self.position = position;
    }

    /// The recording's command markers, in order.
    pub fn chapters(&self) -> Vec<Chapter> {
        self.cast
            .events
            .iter()
            .zip(&self.times)
            .filter_map(|(event, time)| match &event.data {
                EventData::Marker(label) => Some(Chapter {
                    label: label.clone(),
                    time: *time,
                }),
                EventData::Output(_) | EventData::Input(_) | EventData::Resize { .. } => None,
            })
            .collect()
    }

    /// Seeks to the first command after the current position. Returns whether there was one.
    pub fn next_chapter(&mut self) -> bool {
        let position = self.position;
        match self
            .chapters()
            .into_iter()
            .find(|chapter| chapter.time > position + 1e-6)
        {
            Some(chapter) => {
                self.seek(chapter.time);
                true
            }
            None => false,
        }
    }

    /// Seeks to the start of the current command, or the previous one when already near its
    /// start, or the beginning of the recording.
    pub fn previous_chapter(&mut self) {
        let position = self.position;
        let time = self
            .chapters()
            .into_iter()
            .rev()
            .find(|chapter| chapter.time < position - PREVIOUS_CHAPTER_GRACE)
            .map_or(0., |chapter| chapter.time);
        self.seek(time);
    }

    fn rebuild_timeline(&mut self) {
        let limit = self.skip_idle.then(|| self.idle_time_limit());
        let cap = |gap: f64| limit.map_or(gap, |limit| gap.min(limit)).max(0.);
        self.times.clear();
        let (mut raw_prev, mut time) = (0., 0.);
        for event in &self.cast.events {
            time += cap(event.time - raw_prev);
            raw_prev = event.time;
            self.times.push(time);
        }
        self.duration = time + cap(self.cast.duration() - raw_prev);
    }

    /// Maps playback time to recording time.
    fn recording_time_of(&self, time: f64) -> f64 {
        self.map_time(time, &self.times, |index| self.cast.events[index].time)
    }

    /// Maps recording time to playback time.
    fn playback_time_of(&self, raw: f64) -> f64 {
        let raw_times: Vec<f64> = self.cast.events.iter().map(|event| event.time).collect();
        self.map_time(raw, &raw_times, |index| self.times[index])
    }

    /// Finds the last event at or before `time` on the `from` timeline and returns the
    /// corresponding time on the other timeline, plus the remaining offset capped at the gap.
    fn map_time(&self, time: f64, from: &[f64], to: impl Fn(usize) -> f64) -> f64 {
        let index = from.partition_point(|event_time| *event_time <= time);
        if index == 0 {
            return time.min(self.cast.events.first().map_or(time, |_| to(0)));
        }
        let base_from = from[index - 1];
        let base_to = to(index - 1);
        let offset = time - base_from;
        let gap_to = if index < from.len() {
            to(index) - base_to
        } else {
            f64::INFINITY
        };
        base_to + offset.min(gap_to)
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn title(&self) -> Option<&str> {
        self.cast.header.title.as_deref()
    }

    /// Fraction of the recording played, from 0 to 1.
    pub fn progress(&self) -> f32 {
        let duration = self.duration();
        if duration <= 0. {
            1.
        } else {
            (self.position / duration).clamp(0., 1.) as f32
        }
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.screen.screen()
    }

    /// The visible screen as one string per row, trailing blanks trimmed.
    pub fn screen_lines(&self) -> Vec<String> {
        let screen = self.screen.screen();
        let (_, cols) = screen.size();
        screen
            .rows(0, cols)
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    pub fn play(&mut self) {
        if self.state == PlaybackState::Ended {
            self.seek(0.);
        }
        self.state = PlaybackState::Playing;
    }

    pub fn pause(&mut self) {
        if self.state == PlaybackState::Playing {
            self.state = PlaybackState::Paused;
        }
    }

    pub fn toggle_play_pause(&mut self) {
        match self.state {
            PlaybackState::Playing => self.pause(),
            PlaybackState::Paused | PlaybackState::Ended => self.play(),
        }
    }

    pub fn restart(&mut self) {
        self.seek(0.);
        self.state = PlaybackState::Playing;
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(SPEEDS[0], SPEEDS[SPEEDS.len() - 1]);
    }

    /// Moves to the next speed in [`SPEEDS`], wrapping to the slowest.
    pub fn cycle_speed(&mut self) {
        let next = SPEEDS
            .iter()
            .copied()
            .find(|speed| *speed > self.speed + f64::EPSILON)
            .unwrap_or(SPEEDS[0]);
        self.speed = next;
    }

    /// Advances playback by `elapsed` wall-clock seconds, scaled by the speed.
    pub fn advance(&mut self, elapsed: f64) {
        if self.state != PlaybackState::Playing {
            return;
        }
        let target = self.position + elapsed.max(0.) * self.speed;
        self.apply_until(target);
    }

    /// Jumps to `time` seconds, keeping the play/pause state unless the end is reached.
    pub fn seek(&mut self, time: f64) {
        let time = time.clamp(0., self.duration());
        if time < self.position {
            self.screen = new_screen(&self.cast);
            self.next_event = 0;
            self.position = 0.;
        }
        self.restore_snapshot_before(time);
        let was_playing = self.state == PlaybackState::Playing;
        self.state = PlaybackState::Paused;
        self.apply_until(time);
        if self.state != PlaybackState::Ended && was_playing {
            self.state = PlaybackState::Playing;
        }
    }

    pub fn seek_to_fraction(&mut self, fraction: f32) {
        self.seek(f64::from(fraction.clamp(0., 1.)) * self.duration());
    }

    fn apply_until(&mut self, target: f64) {
        while let Some(event) = self.cast.events.get(self.next_event) {
            if self.times[self.next_event] > target {
                break;
            }
            match &event.data {
                EventData::Output(text) => self.screen.process(text.as_bytes()),
                EventData::Resize { cols, rows } => self.screen.screen_mut().set_size(*rows, *cols),
                EventData::Input(_) | EventData::Marker(_) => {}
            }
            self.next_event += 1;
            self.maybe_snapshot();
        }
        let duration = self.duration();
        if target >= duration {
            self.position = duration;
            self.state = PlaybackState::Ended;
        } else {
            self.position = target;
        }
    }
}

impl Player {
    /// Jumps to the latest snapshot at or before `time` when it is ahead of the current screen.
    fn restore_snapshot_before(&mut self, time: f64) {
        let usable = self
            .snapshots
            .partition_point(|snapshot| self.times[snapshot.next_event - 1] <= time);
        let Some(snapshot) = usable.checked_sub(1).map(|index| &self.snapshots[index]) else {
            return;
        };
        if snapshot.next_event <= self.next_event {
            return;
        }
        let mut screen = new_screen(&self.cast);
        *screen.screen_mut() = snapshot.screen.clone();
        self.screen = screen;
        self.next_event = snapshot.next_event;
        self.position = self.times[snapshot.next_event - 1];
    }

    fn maybe_snapshot(&mut self) {
        let (last_event, last_time) = self.snapshots.last().map_or((0, 0.), |snapshot| {
            (snapshot.next_event, self.times[snapshot.next_event - 1])
        });
        if self.next_event <= last_event {
            return;
        }
        let time = self.times[self.next_event - 1];
        if time - last_time >= SNAPSHOT_INTERVAL_SECONDS
            || self.next_event - last_event >= SNAPSHOT_INTERVAL_EVENTS
        {
            self.snapshots.push(Snapshot {
                next_event: self.next_event,
                screen: self.screen.screen().clone(),
            });
        }
    }
}

/// Elapsed and total time, e.g. `0:05 / 1:05`. Elapsed time counts whole seconds so it doesn't
/// run ahead of the screen.
pub fn format_playback_time(position: f64, duration: f64) -> String {
    let elapsed = if position >= duration {
        duration.round()
    } else {
        position.floor().min(duration.round())
    };
    format!(
        "{} / {}",
        format_duration(elapsed),
        format_duration(duration)
    )
}

fn new_screen(cast: &Cast) -> vt100::Parser {
    vt100::Parser::new(
        cast.header.height.max(1),
        cast.header.width.max(1),
        SCROLLBACK_LINES,
    )
}

#[cfg(test)]
#[path = "player_tests.rs"]
mod tests;
