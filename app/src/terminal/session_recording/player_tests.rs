use super::*;
use crate::terminal::session_recording::asciicast::{Event, Header};

fn output(time: f64, text: &str) -> Event {
    Event {
        time,
        data: EventData::Output(text.to_owned()),
    }
}

fn cast() -> Cast {
    Cast {
        header: Header::new(20, 3),
        events: vec![
            output(1., "one\r\n"),
            output(2., "two\r\n"),
            Event {
                time: 3.,
                data: EventData::Resize { cols: 30, rows: 4 },
            },
            output(4., "three"),
        ],
    }
}

fn visible_text(player: &Player) -> String {
    player
        .screen_lines()
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("|")
}

#[test]
fn starts_paused_and_ignores_time_until_played() {
    let mut player = Player::new(cast());
    assert_eq!(player.state(), PlaybackState::Paused);
    player.advance(10.);
    assert_eq!(player.position(), 0.);
    assert_eq!(visible_text(&player), "");
}

#[test]
fn applies_events_as_time_passes() {
    let mut player = Player::new(cast());
    player.play();
    player.advance(1.5);
    assert_eq!(visible_text(&player), "one");
    player.advance(1.);
    assert_eq!(visible_text(&player), "one|two");
    assert_eq!(player.screen().size(), (3, 20));
    player.advance(0.6);
    assert_eq!(player.screen().size(), (4, 30));
}

#[test]
fn speed_scales_time_and_cycles() {
    let mut player = Player::new(cast());
    player.set_speed(2.);
    player.play();
    player.advance(1.);
    assert_eq!(player.position(), 2.);
    player.set_speed(10.);
    assert_eq!(player.speed(), 4.);
    player.cycle_speed();
    assert_eq!(player.speed(), 0.5);
    player.cycle_speed();
    assert_eq!(player.speed(), 1.);
}

#[test]
fn ends_at_duration_and_replays_from_start() {
    let mut player = Player::new(cast());
    player.play();
    player.advance(100.);
    assert_eq!(player.state(), PlaybackState::Ended);
    assert_eq!(player.position(), 4.);
    assert_eq!(player.progress(), 1.);
    assert_eq!(visible_text(&player), "one|two|three");

    player.toggle_play_pause();
    assert_eq!(player.state(), PlaybackState::Playing);
    assert_eq!(player.position(), 0.);
    assert_eq!(visible_text(&player), "");
}

#[test]
fn seeking_backwards_rebuilds_the_screen() {
    let mut player = Player::new(cast());
    player.seek(4.);
    assert_eq!(player.state(), PlaybackState::Ended);
    player.seek(1.);
    assert_eq!(player.state(), PlaybackState::Paused);
    assert_eq!(visible_text(&player), "one");
    assert_eq!(player.screen().size(), (3, 20));
    player.seek_to_fraction(0.5);
    assert_eq!(player.position(), 2.);
    assert_eq!(visible_text(&player), "one|two");
}

#[test]
fn seek_keeps_playing_and_restart_plays() {
    let mut player = Player::new(cast());
    player.play();
    player.seek(2.5);
    assert_eq!(player.state(), PlaybackState::Playing);
    player.pause();
    player.restart();
    assert_eq!(player.state(), PlaybackState::Playing);
    assert_eq!(player.position(), 0.);
}
