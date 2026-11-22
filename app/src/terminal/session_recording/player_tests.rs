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

fn marker(time: f64, label: &str) -> Event {
    Event {
        time,
        data: EventData::Marker(label.to_owned()),
    }
}

/// Commands at 1s and 31s with a 29.5s idle gap before the second.
fn cast_with_markers() -> Cast {
    Cast {
        header: Header::new(20, 3),
        events: vec![
            marker(1., "ls"),
            output(1.5, "a b\r\n"),
            marker(31., "make test"),
            output(32., "ok\r\n"),
        ],
    }
}

#[test]
fn skip_idle_caps_gaps_at_the_limit() {
    let mut player = Player::new(cast_with_markers());
    assert!(player.skip_idle());
    assert_eq!(player.idle_time_limit(), DEFAULT_IDLE_TIME_LIMIT);
    // 1 + 0.5 + 2 (capped from 29.5) + 1
    assert_eq!(player.duration(), 4.5);
    player.set_skip_idle(false);
    assert_eq!(player.duration(), 32.);

    let mut header = Header::new(20, 3);
    header.idle_time_limit = Some(0.5);
    let player = Player::new(Cast {
        header,
        ..cast_with_markers()
    });
    assert_eq!(player.duration(), 2.);
}

#[test]
fn toggling_skip_idle_keeps_the_place() {
    let mut player = Player::new(cast_with_markers());
    player.seek(3.5);
    assert_eq!(visible_text(&player), "a b");
    player.set_skip_idle(false);
    assert_eq!(player.position(), 31.);
    player.set_skip_idle(true);
    assert_eq!(player.position(), 3.5);
}

#[test]
fn chapters_navigate_between_commands() {
    let mut player = Player::new(cast_with_markers());
    let chapters: Vec<_> = player
        .chapters()
        .into_iter()
        .map(|chapter| (chapter.label, chapter.time))
        .collect();
    assert_eq!(
        chapters,
        vec![("ls".to_owned(), 1.), ("make test".to_owned(), 3.5)]
    );

    assert!(player.next_chapter());
    assert_eq!(player.position(), 1.);
    assert!(player.next_chapter());
    assert_eq!(player.position(), 3.5);
    assert_eq!(visible_text(&player), "a b");
    assert!(!player.next_chapter());

    // Within a second of a command's start, previous goes to the command before it.
    player.seek(4.);
    player.previous_chapter();
    assert_eq!(player.position(), 1.);
    player.seek(3.);
    player.previous_chapter();
    assert_eq!(player.position(), 1.);
    player.previous_chapter();
    assert_eq!(player.position(), 0.);
}

#[test]
fn recordings_without_markers_have_no_chapters() {
    let mut player = Player::new(cast());
    assert!(player.chapters().is_empty());
    assert!(!player.next_chapter());
    player.previous_chapter();
    assert_eq!(player.position(), 0.);
}

/// A long recording with colors, cursor moves, a resize, and markers, so seeking crosses several
/// snapshots.
fn long_cast() -> Cast {
    let mut events = Vec::new();
    for i in 0..400 {
        let time = f64::from(i) * 0.25;
        let text = match i % 4 {
            0 => format!("\x1b[3{}mline {i}\x1b[0m\r\n", i % 8),
            1 => format!("\x1b[2;5H#{i}"),
            2 => "\x1b[K partial".to_owned(),
            _ => "\r\n".to_owned(),
        };
        events.push(output(time, &text));
        if i == 150 {
            events.push(Event {
                time,
                data: EventData::Resize { cols: 50, rows: 8 },
            });
        }
        if i % 97 == 0 {
            events.push(Event {
                time,
                data: EventData::Marker(format!("cmd {i}")),
            });
        }
    }
    Cast {
        header: Header::new(40, 6),
        events,
    }
}

fn played_to(time: f64) -> Vec<u8> {
    let mut player = Player::new(long_cast());
    player.play();
    player.advance(time);
    player.screen().contents_formatted()
}

#[test]
fn recording_seek_matches_playing_to_the_same_time() {
    let mut player = Player::new(long_cast());
    // Play through once so every snapshot exists, then seek around in both directions.
    player.seek(player.duration());
    assert!(player.snapshots.len() >= 10);
    for time in [97.3, 3.1, 50., 12.6, 0., 37.5, 99.6, 6.0, 74.2] {
        player.seek(time);
        assert_eq!(player.position(), time);
        assert_eq!(
            player.screen().contents_formatted(),
            played_to(time),
            "screen differs at {time}s"
        );
    }
}

#[test]
fn recording_seek_uses_snapshots_after_skip_idle_changes() {
    let mut player = Player::new(long_cast());
    player.seek(player.duration());
    player.set_skip_idle(false);
    player.seek(10.);
    player.seek(80.);
    assert_eq!(player.screen().contents_formatted(), played_to(80.));
}

#[test]
fn recording_playback_time_formatting() {
    assert_eq!(format_playback_time(0., 65.), "0:00 / 1:05");
    assert_eq!(format_playback_time(4.9, 65.), "0:04 / 1:05");
    assert_eq!(format_playback_time(65., 65.), "1:05 / 1:05");
    assert_eq!(format_playback_time(64.8, 64.6), "1:05 / 1:05");
    assert_eq!(format_playback_time(3725., 3725.), "1:02:05 / 1:02:05");
}
