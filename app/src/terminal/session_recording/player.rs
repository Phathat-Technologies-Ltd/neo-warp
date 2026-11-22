//! Replays an asciicast into a vt100 screen. Time is supplied by the caller so the playback logic
//! stays deterministic.

use super::asciicast::{Cast, EventData};

pub const SPEEDS: [f64; 5] = [0.5, 1., 1.5, 2., 4.];
const SCROLLBACK_LINES: usize = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Playing,
    Paused,
    Ended,
}

pub struct Player {
    cast: Cast,
    screen: vt100::Parser,
    next_event: usize,
    /// Seconds into the recording.
    position: f64,
    speed: f64,
    state: PlaybackState,
}

impl Player {
    /// A player paused at the start.
    pub fn new(cast: Cast) -> Self {
        let screen = new_screen(&cast);
        Self {
            cast,
            screen,
            next_event: 0,
            position: 0.,
            speed: 1.,
            state: PlaybackState::Paused,
        }
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    pub fn position(&self) -> f64 {
        self.position
    }

    pub fn duration(&self) -> f64 {
        self.cast.duration()
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
            if event.time > target {
                break;
            }
            match &event.data {
                EventData::Output(text) => self.screen.process(text.as_bytes()),
                EventData::Resize { cols, rows } => self.screen.screen_mut().set_size(*rows, *cols),
                EventData::Input(_) | EventData::Marker(_) => {}
            }
            self.next_event += 1;
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
