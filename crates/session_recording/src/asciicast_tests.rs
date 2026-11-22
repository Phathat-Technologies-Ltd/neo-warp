use std::io::Cursor;

use super::*;

fn header() -> Header {
    Header {
        timestamp: Some(1_700_000_000),
        title: Some("zsh ~/dev".to_owned()),
        env: BTreeMap::from([
            ("SHELL".to_owned(), "/bin/zsh".to_owned()),
            ("TERM".to_owned(), "xterm-256color".to_owned()),
        ]),
        ..Header::new(80, 24)
    }
}

fn write(build: impl FnOnce(&mut CastWriter<Vec<u8>>)) -> String {
    let mut writer = CastWriter::new(Vec::new(), &header()).unwrap();
    build(&mut writer);
    String::from_utf8(writer.into_inner().unwrap()).unwrap()
}

#[test]
fn round_trips_header_and_events() {
    let text = write(|writer| {
        writer
            .write_output(0.5, b"hello \x1b[1mworld\x1b[0m\r\n")
            .unwrap();
        writer.write_resize(1.25, 100, 30).unwrap();
        writer.write_output(2., "caf\u{e9}".as_bytes()).unwrap();
    });
    let cast = parse(Cursor::new(text)).unwrap();
    assert_eq!(cast.header, header());
    assert_eq!(
        cast.events,
        vec![
            Event {
                time: 0.5,
                data: EventData::Output("hello \x1b[1mworld\x1b[0m\r\n".to_owned()),
            },
            Event {
                time: 1.25,
                data: EventData::Resize {
                    cols: 100,
                    rows: 30
                },
            },
            Event {
                time: 2.,
                data: EventData::Output("caf\u{e9}".to_owned()),
            },
        ]
    );
    assert_eq!(cast.duration(), 2.);
}

#[test]
fn writes_spec_shaped_lines() {
    let text = write(|writer| {
        writer.write_output(0.1234567, b"a").unwrap();
        writer.write_resize(1., 90, 20).unwrap();
    });
    let lines: Vec<_> = text.lines().collect();
    assert!(lines[0].starts_with(r#"{"version":2,"width":80,"height":24,"timestamp":1700000000"#));
    assert_eq!(lines[1], r#"[0.123457,"o","a"]"#);
    assert_eq!(lines[2], r#"[1.0,"r","90x20"]"#);
}

#[test]
fn holds_split_utf8_until_complete() {
    let euro = "\u{20ac}".as_bytes();
    let text = write(|writer| {
        writer.write_output(0.1, &euro[..1]).unwrap();
        writer.write_output(0.2, &euro[1..]).unwrap();
    });
    let cast = parse(Cursor::new(text)).unwrap();
    assert_eq!(
        cast.events,
        vec![Event {
            time: 0.2,
            data: EventData::Output("\u{20ac}".to_owned()),
        }]
    );
}

#[test]
fn replaces_invalid_utf8() {
    let text = write(|writer| writer.write_output(0., b"a\xffb").unwrap());
    let cast = parse(Cursor::new(text)).unwrap();
    assert_eq!(
        cast.events[0].data,
        EventData::Output("a\u{fffd}b".to_owned())
    );
}

#[test]
fn parses_asciinema_file_and_skips_unknown_codes() {
    let text = concat!(
        r#"{"version": 2, "width": 10, "height": 5, "duration": 9.5}"#,
        "\n",
        r#"[0.1, "o", "x"]"#,
        "\n\n",
        r#"[0.2, "i", "ls"]"#,
        "\n",
        r#"[0.3, "z", "?"]"#,
        "\n",
        r#"[0.4, "m", "chapter"]"#,
        "\n",
    );
    let cast = parse(Cursor::new(text)).unwrap();
    assert_eq!(cast.events.len(), 3);
    assert_eq!(cast.events[1].data, EventData::Input("ls".to_owned()));
    assert_eq!(cast.duration(), 9.5);
}

#[test]
fn rejects_other_versions_and_bad_events() {
    assert!(parse(Cursor::new(r#"{"version": 1, "width": 1, "height": 1}"#)).is_err());
    let bad_resize = "{\"version\": 2, \"width\": 1, \"height\": 1}\n[0, \"r\", \"oops\"]\n";
    assert!(parse(Cursor::new(bad_resize)).is_err());
    assert!(parse(Cursor::new("")).is_err());
}

#[test]
fn summary_reports_last_event_time() {
    let text = write(|writer| {
        writer.write_output(0.5, b"a").unwrap();
        writer.write_output(3.75, b"b").unwrap();
    });
    let (summary_header, duration) = read_summary(Cursor::new(text)).unwrap();
    assert_eq!(summary_header, header());
    assert_eq!(duration, 3.75);
}
