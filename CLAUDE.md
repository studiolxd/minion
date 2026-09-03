# Minion — working notes for Claude

Minion is a native macOS menu-bar app that listens all the time, recognises
**Spanish** speech offline, checks that the speaker is its owner, and runs
commands ("minion, abre Chrome", "minion, cierra la pestaña", "minion, ¿qué
hora es?"). Everything lives in `minion/`. This file records what the code
cannot: the decisions, the dead ends, and the routine.

## How we got here (and what was discarded)

1. **Talon Voice** with a custom command set (no `talonhub/community`).
   Abandoned: Talon's Conformer engine is English-only; there is no Spanish
   model. `~/.talon` is gone.
2. **Handy + Python bridge** (`voz/comandos.py`): Handy recognised Spanish,
   a script executed the text. Abandoned: two apps, a global shortcut, no
   always-on mode worth having. Handy is uninstalled.
3. **Minion**, in Rust, first named "Oyente". This is the live project.

Do not resurrect 1 or 2. The user chose Rust deliberately over Python/Go.

## Rules the user has set

- **Code, comments, commits, docs in English.** Spanish only for what is
  spoken or shown to the user (phrases, menu items, log lines are English).
- **Natural language first**: "cierra la ventana" is the primary phrase,
  "cerrar ventana" is also accepted. Add both when adding a command.
- No destructive actions from a single phrase. Terminal commands are typed,
  never followed by Enter.
- Never invent aliases from guesswork: alias what the recogniser **actually
  wrote** in the log (`unknown  «…»`). That is what `minion learn` is for.
- Preferences UI: use the `spacing` module + `Layout` in
  `src/preferences.rs`; never hand-tune coordinates.
- Menu: "Escuchar/Pausar" is one toggle; "Ayuda" sits above "Salir".
- Commit with clear messages (imperative, say why). The user asks for commits
  explicitly ("haz commits"); do them after each working change.

## Build, test, install

`cargo` is **not on PATH** in the Claude shell. Always:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:/opt/homebrew/bin:$PATH"
cd /Users/suvi/Dev/talon/minion
cargo test                 # ~132 tests, must all pass
cargo clippy --all-targets # 6 pre-existing warnings (new clippy); add none
./build-app.sh             # cargo build --release + Minion.app, signed with
                           # the user's Apple Development certificate
./install.sh               # quits the running copy, copies to /Applications,
                           # writes the launchd plist, relaunches
```

Build+install takes a few minutes: run it in the background. The app must be
**signed with the real certificate** (not ad-hoc) or macOS revokes the
Accessibility permission on every rebuild.

Models (Parakeet TDT 0.6b v3 int8 + ECAPA speaker model) download on first
run to Application Support; the bundle is ~29 MB.

## Where things live at runtime

| What | Path |
|---|---|
| Config | `~/Library/Application Support/Minion/config.toml` |
| Voice profile | `~/Library/Application Support/Minion/voice.txt` |
| Models | `~/Library/Application Support/Minion/model/` |
| Single-instance lock | `~/Library/Application Support/Minion/running.lock` |
| Log | `~/Library/Logs/minion.log` |
| Recordings (if `save_recordings = true`) | `~/Library/Application Support/Minion/recordings/` |

Config uses `deny_unknown_fields`: a key in the wrong table silently
invalidates the whole file. Top-level keys go **before** `[audio]`.

`save_recordings` is currently **off**. Turn it on (and tell the user) before
a test round whose audio you will need.

## Reading the log

```
ran      «Minion Chrome.»  ->  abrir Chrome  [100% · 1.6s audio · 142 ms]
unknown  «Minium so fuddy.»  ->  not understood      # wake word ok, command not
heard    1.5s of speech, not addressed to me          # wake word not recognised
heard    4.8s in another voice (-0.01)                # speaker check failed
voice    matched at 0.58                              # speaker check passed
blank    1.8s of audio, nothing recognised            # Parakeet returned nothing
```

Diagnose from the log before touching code. The last big round (2026-09-03)
showed the wake word at 17/18 and **all** failures in the second word:
English app names ("Chrome" → grum/crum/crumb, "Safari" → so fuddy) and the
recogniser drifting into English mid-phrase ("minimized the ventana").
"terminal", a Spanish word, never failed. A longer wake word ("ey minion")
was considered and rejected on that data.

## Architecture (src/)

- `main.rs` — orchestration: `listen_and_obey()`, menu bar, log, flock,
  permission report, idle model unload (5 min → 405 MB).
- `audio.rs` — cpal capture, resample, energy VAD with adaptive noise floor,
  320 ms preroll (without it "Chrome" lost its first consonant), segmenter.
- `commands.rs` — vocabulary: `DEFAULT_WAKE_WORDS`, `APPS` (with aliases),
  `COMMANDS`, `CONTEXTUAL_COMMANDS`, `NUMBERED`; `decide_in()` order:
  dictation → mode/undo → contextual → COMMANDS → questions → numbered →
  music → user aliases → websites → apps. Wake word is matched fuzzily
  (≤2 edits, 3 shared leading letters; 2 let "millón" through) and a wake
  word split in two ("Mini on") is rejoined.
- `text.rs` — normalise, keyword F1 similarity, `words_match` (≤1 edit, or
  a shared stem ≥6 letters covering ¾ of the shorter word), `edits_between`.
- `spanish.rs` — filler words, verb canonicalisation (explicit table, no
  stemmer on purpose).
- `speaker.rs` — ECAPA-TDNN embeddings, cosine similarity, threshold
  **0.32** (measured on real mic audio: worst 0.38, avg 0.57; 0.45 rejected
  the owner). `fbank.rs` is the Kaldi-compatible mel frontend.
- `answers.rs` / `speech.rs` — spoken answers (time, date, battery, volume,
  help) via the system synthesiser.
- `preferences.rs` — AppKit window; controls are **polled** by an NSTimer in
  `NSRunLoopCommonModes` (not Objective-C targets — documented at the top).
- `hotkey.rs` — CGEventTap on its own thread (an `NSEvent` global monitor on
  the main loop broke the menu). Default ⌥Space pauses/resumes.
- `actions.rs` — key codes (positional! Spanish ISO layout breaks `cmd-[`
  etc.), open/quit apps, type text, shortcut parsing.
- `learn.rs` / `enroll.rs` — turn `unknown` log lines into aliases; record
  the voice profile. `journal.rs`, `config.rs`, `icon.rs`, `models.rs`,
  `startup.rs` are what their names say.
- `assets/*.svg` — the face. Asleep keeps the **same smile**, only the eye
  closes.

## Hard-won facts

- Tests must never touch real user data: `save_profile_for` and the log
  writer no-op under `cfg!(test)`. Keep it that way.
- Accessory (`LSUIElement`) apps must `activateIgnoringOtherApps` before
  showing a window.
- Sliders only feel live if the polling timer runs in
  `NSRunLoopCommonModes`.
- ONNX `disable_prepacking` halved memory (1830 → 934 MB).
- "Start at login" only writes the plist; bootstrapping it launched a
  second copy. `flock` guards against duplicates anyway.
- Speaker verification bugs hide behind synthetic tests (0.84 same-voice on
  clean audio). Measure on real recordings before changing thresholds.

## Open items

- `blank` lines: voice matched, Parakeet returned no text. Seen a few times
  per round; if it grows, lengthen the preroll.
- Not planned unless asked: Developer ID notarisation, Silero VAD, lowering
  the 405 MB idle floor.
