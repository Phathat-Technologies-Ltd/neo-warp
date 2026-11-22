# Recognize my voice (speaker verification) — Product

## Problem

Hands-free voice input and conversation mode keep the microphone open. Anything it hears becomes a
prompt: a colleague talking nearby, a video playing, or the assistant's own reply coming back out
of the speakers (TTS echo). Misheard prompts can auto-send.

## Behavior

- **Settings > Voice > Recognize My Voice** (shown when hands-free voice is available and the
  `voice_speaker_id` org flag is on; hidden otherwise).
  - **Recognize My Voice** toggle (default on). It has no effect until the user enrolls.
  - **Voiceprint** row:
    - Not enrolled: explains the feature and that the voiceprint never leaves the computer;
      button **Enroll**.
    - Enrolling: "Phrase N of 3. Read aloud: “…”" with **Cancel**. Each phrase records for about
      5.5 seconds, then the next phrase appears. After the third, "Creating your voiceprint…".
    - Failed (for example nothing was heard): the reason and **Try Again**.
    - Enrolled: button **Re-enroll**.
  - **Delete Voiceprint** (when enrolled): removes it; every voice is accepted again.
  - **Sensitivity** (when enrolled): Strict / Normal (default) / Loose.
  - **Speaker Model** row: download, cancel, or remove the model, like the other voice models.
- **Consent**: clicking Enroll without the speaker model shows the usual consent toast with the
  download size (30 MB). Nothing downloads until the user clicks Download. After the download the
  user clicks Enroll again.
- Enrolling stops hands-free input first so the two don't share the microphone.
- **Strictly optional.** Without an enrolled voiceprint, with the toggle or flag off, or without the
  speaker model, hands-free and conversation mode work exactly as before: no check, no prompt, and
  nothing blocks use. Only the speaker filter is inactive. The speaker model is never requested
  outside the Enroll flow.
- **Enforcement** (enrolled, toggle on, model installed, when a hands-free session starts):
  - Each finished utterance is compared with the voiceprint. If it doesn't match, its text is
    dropped: it is not inserted, not sent, and it doesn't start auto-send. The live partial text
    shown while speaking is cleared.
  - The mic indicator briefly shows "● Mic on · Not you — ignored" (3 seconds). There is no
    toast or sound.
  - The same applies in conversation mode, so the assistant's own voice and approval answers from
    other people are ignored.
  - **Stop phrases always pass.** "Stop" (and the other stop phrases) is never checked against the
    voiceprint, so the user can always silence the assistant, even if the check misses them
    (noisy room, a cold). The assistant doesn't say "stop" itself, so echo can't misuse this.
  - If the check can't run (for example the model files were deleted), utterances are let through
    rather than silently dropping the user's speech.
  - Changes to the toggle, sensitivity, or voiceprint apply from the next hands-free session.
- **Reduce Background Noise** toggle (default on, shown when hands-free voice is available):
  suppresses steady background noise (fans, hum, keyboard) in the microphone audio before speech
  detection.

## Privacy

- The voiceprint is a 512-number vector computed on this computer. It is stored only in the app's
  data directory (`voice/voiceprint.json`) with owner-only permissions (0600) and is never synced
  or sent anywhere. Audio is never written to disk.
- Delete Voiceprint removes the file. Removing the speaker model leaves the voiceprint in place
  but turns checking off until the model is downloaded again.

## Gating

Org product-area flag `voice_speaker_id` (default ON). It requires `voice_hands_free`; turning off
`voice_input` or `voice_hands_free` also turns it off.

## Limitations

- Thresholds are provisional; see TECH.md. Very short utterances ("yes", "no") give weaker
  matches; Loose helps if the user gets ignored too often.
- One voiceprint per computer user account.
- Partial text from another speaker may appear briefly before the final check clears it.
