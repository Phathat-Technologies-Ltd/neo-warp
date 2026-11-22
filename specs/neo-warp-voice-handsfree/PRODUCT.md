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
- **Footer mic and mode dropdown (GUI)**: the agent input footer and the terminal (agent view)
  input footer share one mic button with a small chevron dropdown next to it. The dropdown picks the
  dictation mode, shared by every footer: "Push to talk / click to record" (default; one recording
  per click, the existing voice input) or "Hands-free". Clicking the mic starts the selected mode.
  While hands-free has the microphone open, the mic is drawn in red and a click turns hands-free
  off, whatever mode is selected. The tooltip names the action and its shortcut (the hold-to-talk
  key for recording, cmd-shift-H for hands-free). The dropdown is hidden when `VoiceHandsFree` is
  off or local models aren't served, and the mic is hidden when voice input is off. In a CLI agent
  session the mic keeps its PTY recording behavior and only stops a running hands-free session.
- **Listening indicator**: while hands-free is on, the input shows a pulsing mic glyph with a level
  meter and three states: Listening (idle), Hearing (speech detected), and Transcribing.
- **Privacy indicator**: whenever the microphone is open, a red dot and a "Mic on" label stay in the
  window header or TUI status line, whatever has focus. All audio stays on the device and nothing is
  written to disk.
- **Pause outside Warp**: when Warp is not the active app, hands-free (and conversation mode)
  pauses: the microphone closes, so the system mic indicator turns off; an utterance in progress
  is discarded, and nothing said meanwhile is inserted or sent. The indicator reads "Mic paused".
  Listening resumes on its own when the user returns to Warp. System sleep turns hands-free off.
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
- **Routing in the terminal input**: dictation is mostly natural language. When an utterance starts
  an empty terminal input:
  - Natural language goes to the agent: the input switches to agent mode, and auto-send sends it.
  - A command mixed with a request ("run git status and tell me what changed") also goes to the
    agent.
  - A clear command (it begins with a known executable such as `git` or `ls`, optionally after a
    spoken "run", "command", or "execute", which is removed, has no English words such as "and"
    or "tell", and does not end with a question mark) switches the input to shell mode. If it is
    read-only (`ls`, `pwd`, `cat`, `head`, `tail`, `wc`, `grep`, `rg`, `which`, `git status`,
    `git log`, `git diff`, `git show`, and similar, with no pipes, redirects, `;`, `&`, or `$`),
    auto-send runs it after the pause. Anything else stays in the input, with a short toast
    explaining that it waits for a spoken "send" or Enter.
  - A mode the user picked by hand (cmd-I or a `!` prefix) is kept.
  Sending a hands-free input does exactly what pressing Enter does, so prompts go to the agent and
  commands to the shell.
- **Wake word**: future work (for example "Hey Warp" with a tiny keyword-spotting model). Not in v1.
- **Models**: the VAD model (2 MB) downloads after the same consent dialog, with the same live
  progress, as the speech model. The dialog lists both models. Hands-free cannot start until both
  are installed.
- **Gating**: org flag `VoiceHandsFree` (default ON in org builds). It requires `VoiceInput`. When
  the flag is off, the toggle, palette entry, settings rows, and keybinding are hidden.

- **One target**: the session is one global microphone, but only one input owns it: the focused
  input of the active tab or pane in the active window. Only that input shows "Mic on", the live
  footer state (Listening, Hearing, Thinking, Speaking), and receives transcripts; every other
  input's footer shows the idle buttons. Focusing another tab, pane, or window moves the
  indicator and the text target there, and a partial transcript left in the old input is removed.

## Non-goals
Speaker identification, cloud STT, and listening that stays on across app restarts.
