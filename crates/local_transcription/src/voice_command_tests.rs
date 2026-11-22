use super::*;

#[test]
fn splits_trailing_commands() {
    let cases = [
        ("Run the tests, send it.", "Run the tests", Some(VoiceCommand::Send)),
        ("run the tests send", "run the tests", Some(VoiceCommand::Send)),
        ("Send.", "", Some(VoiceCommand::Send)),
        ("delete it, scratch that", "delete it", Some(VoiceCommand::Cancel)),
        ("cancel", "", Some(VoiceCommand::Cancel)),
        ("first step, new line", "first step", Some(VoiceCommand::NewLine)),
        ("first step newline", "first step", Some(VoiceCommand::NewLine)),
        // Command words elsewhere in the utterance are just words.
        ("send the logs to Bob", "send the logs to Bob", None),
        ("cancel the job please", "cancel the job please", None),
        ("  plain text  ", "plain text", None),
    ];
    for (text, rest, command) in cases {
        assert_eq!(
            split_voice_command(text),
            (rest.to_owned(), command),
            "{text:?}"
        );
    }
}
