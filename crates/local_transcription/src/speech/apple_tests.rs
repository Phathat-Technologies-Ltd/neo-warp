use super::*;

fn voice(name: &str, language: &str, quality: AppleVoiceQuality, personal: bool) -> AppleVoice {
    AppleVoice {
        identifier: format!("id.{name}"),
        name: name.to_owned(),
        language: language.to_owned(),
        quality,
        personal,
    }
}

#[test]
fn ranks_voices_of_the_language_best_first() {
    let voices = vec![
        voice("Samantha", "en-US", AppleVoiceQuality::Default, false),
        voice("Thomas", "fr-FR", AppleVoiceQuality::Premium, false),
        voice("Me", "en-US", AppleVoiceQuality::Premium, true),
        voice("Zoe", "en-US", AppleVoiceQuality::Premium, false),
        voice("Ava", "en-US", AppleVoiceQuality::Premium, false),
        voice("Daniel", "en_GB", AppleVoiceQuality::Enhanced, false),
        voice("Enga", "eng-XX", AppleVoiceQuality::Premium, false),
        AppleVoice {
            identifier: "com.apple.speech.synthesis.voice.Albert".to_owned(),
            ..voice("Albert", "en-US", AppleVoiceQuality::Default, false)
        },
        AppleVoice {
            identifier: "com.apple.eloquence.en-US.Eddy".to_owned(),
            ..voice("Eddy", "en-US", AppleVoiceQuality::Default, false)
        },
    ];
    let names: Vec<String> = rank_apple_voices(voices, "en")
        .into_iter()
        .map(|voice| voice.name)
        .collect();
    assert_eq!(
        names,
        ["Ava", "Zoe", "Me", "Daniel", "Samantha", "Albert", "Eddy"]
    );
}

#[test]
fn display_name_shows_quality_or_personal_voice() {
    assert_eq!(
        voice("Ava", "en-US", AppleVoiceQuality::Premium, false).display_name(),
        "Ava (Premium, en-US)"
    );
    assert_eq!(
        voice("Me", "en-US", AppleVoiceQuality::Premium, true).display_name(),
        "Me (Personal Voice, en-US)"
    );
}
