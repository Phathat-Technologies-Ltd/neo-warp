# Voice Conversation Mode for neo-warp: Product Spec

Builds on hands-free input (`specs/neo-warp-voice-handsfree`). Conversation mode also speaks the
agent's replies aloud, so a user can drive a terminal task entirely by voice.

## Behavior
- **Toggle**: a "Conversation mode" toggle next to the hands-free mic, a palette entry, and a
  keybinding. Turning conversation mode on also turns on hands-free.
- **Indicators**: Listening, Thinking (request sent, no text yet), and Speaking. They appear in the
  input footer (GUI) and status line (TUI), alongside the mic privacy indicator from hands-free.
- **Speaking replies**: agent text is spoken one sentence at a time as it streams in. Audio should
  start within about 300 ms of the first complete sentence.
- **What is spoken**:
  - In conversation mode the agent opens each reply with one or two plain-English sentences meant
    to be heard (a hidden `<spoken>` summary). Only those are read aloud; the full technical answer
    stays on screen and the summary is not shown.
  - When a reply has no summary, a short readout is spoken instead: the first one or two sentences
    (about 40 words at most) with code, paths, commands, URLs, hashes, lists, tables, and long
    numbers left out. Markdown symbols are never read aloud.
  - Tool calls and their output are summarized, for example "Ran cargo check: 2 errors". Short
    successful output (two lines or less) may be read aloud; large output is never read.
  - Paths, flags, and identifiers are read in a natural way ("dash dash release", "app slash src
    slash lib dot rs"). Long paths are shortened to their last component.
- **Barge-in**: if the user starts talking while the assistant is speaking, playback stops within
  about 150 ms on headphones (about 600 ms on loudspeakers, where the mic also hears the assistant)
  and the new utterance is captured. On loudspeakers, short speech over the assistant is treated
  as echo and dropped, except short stop phrases. Saying "stop" (or "quiet", "enough") while the
  assistant is speaking or thinking ends the speech without sending anything; while it is
  listening, "stop" is an ordinary prompt.
- **Stopping and muting**: Esc stops the readout immediately while a reply is being read (otherwise
  Esc works as usual), as do a "Stop speaking" palette entry and a Stop button next to the mic
  indicator. The rest of that reply is dropped; the next reply is spoken normally. A Mute toggle
  next to the indicator keeps conversation mode on but silences all readout until unmuted.
- **Approvals**: when the agent wants to run a command the risk classification marks as safe, it
  says "I want to run <short command>. Say run it, or no." Saying "yes", "run it", or "approve"
  approves; "no", "skip", or "cancel" rejects. Only speech that starts after the prompt has
  finished playing counts, and after 20 seconds only the screen answers. Risky or unclassified
  commands are announced with "Please approve it on screen" and cannot be approved by voice. Any
  other reply leaves the normal on-screen approval buttons active, and those buttons always work.
- **Speech settings**: the voice (from the chosen model's voices), the rate (0.8x to 1.5x), and a
  "Read code summaries" on/off switch.
- **Model download consent**: the TTS model (about 115 MB) has its own consent dialog that shows its
  size, live progress, and a cancel button. Settings > Voice > Speech Output Model offers delete.
- **Gating**: org flag `VoiceConversation` (default ON in org builds). It requires `VoiceHandsFree`.
  When the flag is off, all of this UI is hidden.

## Non-goals
Voice cloning, multilingual TTS in v1 (English only), and a wake word.
