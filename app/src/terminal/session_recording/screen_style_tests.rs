use std::io::Cursor;

use super::*;
use crate::terminal::session_recording::asciicast;
use crate::terminal::session_recording::player::Player;

fn palette() -> Palette {
    let mut ansi = [ColorU::new(0, 0, 0, 255); 16];
    for (index, color) in ansi.iter_mut().enumerate() {
        *color = ColorU::new(index as u8, 0, 0, 255);
    }
    Palette {
        foreground: ColorU::new(200, 200, 200, 255),
        background: ColorU::new(10, 10, 10, 255),
        ansi,
    }
}

#[test]
fn renders_a_small_cast_with_colors_and_bold() {
    let cast = "{\"version\":2,\"width\":20,\"height\":2}\n\
        [0.1,\"o\",\"\\u001b[31mred\\u001b[0m \\u001b[1;32mok\\u001b[0m\\r\\n\"]\n\
        [0.2,\"o\",\"\\u001b[7minv\\u001b[0m\\u001b[38;2;1;2;3mrgb\\u001b[48;5;196mx\"]\n";
    let mut player = Player::new(asciicast::parse(Cursor::new(cast)).unwrap());
    player.seek(1.);
    let palette = palette();
    let lines = styled_lines(player.screen(), &palette);

    let first: Vec<_> = lines[0]
        .iter()
        .map(|run| (run.text.as_str(), run.foreground, run.bold))
        .collect();
    assert_eq!(
        first,
        vec![
            ("red", palette.ansi[1], false),
            (" ", palette.foreground, false),
            // Bold normal green is drawn bright green.
            ("ok", palette.ansi[10], true),
        ]
    );

    let second = &lines[1];
    assert_eq!(second[0].text, "inv");
    assert_eq!(second[0].foreground, palette.background);
    assert_eq!(second[0].background, Some(palette.foreground));
    assert_eq!(second[1].text, "rgb");
    assert_eq!(second[1].foreground, ColorU::new(1, 2, 3, 255));
    assert_eq!(second[2].text, "x");
    assert_eq!(second[2].background, Some(ColorU::new(255, 0, 0, 255)));
}

#[test]
fn blank_rows_have_no_runs() {
    let mut parser = vt100::Parser::new(3, 10, 0);
    parser.process(b"hi   ");
    let lines = styled_lines(parser.screen(), &palette());
    assert_eq!(lines[0].len(), 1);
    assert_eq!(lines[0][0].text, "hi");
    assert!(lines[1].is_empty());
}
