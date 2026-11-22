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
  - Prose only. Markdown symbols are never read aloud.
  - A code block is replaced by "I wrote a code block in <language>, N lines".
  - Tool calls and their output are summarized, for example "Ran cargo check: 2 errors".
  - Paths, flags, and identifiers are read in a natural way ("dash dash release", "app slash src
    slash lib dot rs"). Long paths are shortened to their last component.
- **Barge-in**: if the user starts talking while the assistant is speaking, playback stops within
  about 150 ms and the new utterance is captured. Saying "stop" ends the speech without sending
  anything.
- **Approvals**: when the agent wants to run a command, it says "I want to run <short command>. Say
  yes or no." Saying "yes", "run it", or "approve" approves; "no", "skip", or "cancel" rejects. Any
  other reply leaves the normal on-screen approval buttons active, and those buttons always work.
- **Speech settings**: the voice (from the chosen model's voices), the rate (0.8x to 1.5x), and a
  "Read code summaries" on/off switch.
- **Model download consent**: the TTS model (about 115 MB) has its own consent dialog that shows its
  size, live progress, and a cancel button. Settings > Voice > Speech Output Model offers delete.
- **Gating**: org flag `VoiceConversation` (default ON in org builds). It requires `VoiceHandsFree`.
  When the flag is off, all of this UI is hidden.

## Non-goals
Voice cloning, multilingual TTS in v1 (English only), and a wake word.
