# Hands-free Voice Input for neo-warp: Product Spec

Extends `specs/neo-warp-voice` (push-to-talk, local Parakeet). Hands-free mode lets the user talk to
the terminal or agent without holding a key. Speech is split into utterances automatically, live
text appears in the input, and the prompt is sent after a pause or when the user says a command
word.

## Scope
The speech is about operating a terminal: commands, paths (`~/dev/warp/app/src`), flags
(`--no-fail-fast`), git terms (rebase, HEAD, origin main), and code identifiers. General dictation
is not a goal.

## Behavior
- **Toggle**: a mic toggle in the input footer (GUI and TUI), a Command Palette entry
  "Enable/Disable hands-free voice", and a keybinding. Off by default in each session.
- **Listening indicator**: while hands-free is on, the input shows a pulsing mic glyph with a level
  meter and three states: Listening (idle), Hearing (speech detected), and Transcribing.
- **Privacy indicator**: whenever the microphone is open, a red dot and a "Mic on" label stay in the
  window header or TUI status line, whatever has focus. All audio stays on the device and nothing is
  written to disk.
- **Segmentation**: a local voice-activity detector splits speech into utterances. A pause under
  about 300 ms keeps the current utterance open; a longer one ends it.
- **Live text**: partial transcripts update in place in the input while the user speaks. When the
  utterance ends, its final text replaces the partial. Typing in the input pauses hands-free capture
  until the input is cleared or sent.
- **Sending**: two options in Settings > Voice:
  - "Auto-send after pause": default 1.2 s, range 0.6 to 3 s, or Off.
  - "Voice commands": default On. Saying "send" or "send it" at the end sends the prompt;
    "cancel" or "scratch that" clears the input; "new line" inserts a newline. Command words count
    only as the last word or words of an utterance, and they are removed from the text.
- **Wake word**: future work (for example "Hey Warp" with a tiny keyword-spotting model). Not in v1.
- **Models**: the VAD model (2 MB) downloads after the same consent dialog, with the same live
  progress, as the speech model. The dialog lists both models. Hands-free cannot start until both
  are installed.
- **Gating**: org flag `VoiceHandsFree` (default ON in org builds). It requires `VoiceInput`. When
  the flag is off, the toggle, palette entry, settings rows, and keybinding are hidden.

## Non-goals
Speaker identification, cloud STT, and listening that stays on across app restarts.
