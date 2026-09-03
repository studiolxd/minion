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
- Dictated text is logged verbatim, on purpose: `minion learn` needs to see
  exactly what was said to turn it into an alias. Do not start redacting or
  truncating it.
- Commands are never gated by voice similarity beyond the single 0.32
  `voice_threshold` check — the user rejected escalating that per command
  (e.g. a higher bar for dictation or quitting apps). One check, one number.

## Build, test, install

`cargo` is **not on PATH** in the Claude shell. Always:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:/opt/homebrew/bin:$PATH"
cd /Users/suvi/Dev/talon/minion
cargo test                 # 383 tests (1 ignored), must all pass
cargo clippy --all-targets # 5 pre-existing warnings (new clippy); add none
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
| Voice profiles | `~/Library/Application Support/Minion/voices/<name>.txt` (the older single `voice.txt` is copied into it on first run and kept) |
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
voice    Ana matched at 0.58                          # speaker check passed
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
  permission report, idle model unload (5 min → 405 MB). Also has `fatal()`
  (log + Spanish `NSAlert` + exit 0, never a non-zero exit that would make
  launchd loop), a panic hook (release runs `panic = "abort"`, so the hook
  is the only record of what killed it), tray-first model download (icon
  exists before the 670 MB fetch starts, so the menu bar is never empty),
  «Reiniciar», and a stateful tooltip (listening/paused, last utterance and
  outcome). The per-utterance state machine (dictation mode, undo, repeat,
  chain splitting) lives in `session.rs` as `Session::interpret()` →
  `Outcome`, pure and unit-tested; `main.rs` matches on the outcome and
  does the IO.
- `audio.rs` — cpal capture, resample through a windowed-sinc anti-alias
  filter before decimating to 16 kHz (the naive decimator folded 8–24 kHz
  into 0–8 kHz, which is where fricatives live — "Chrome" and "Safari"
  were the worst hit), energy VAD with adaptive noise floor, 320 ms preroll
  (without it "Chrome" lost its first consonant), segmenter. Emits
  `Utterance { samples, speech_start, speech_end }` so downstream code can
  isolate the speech from the preroll/silence around it.
- `vocabulary.rs` — the vocabulary is TOML, not Rust: one file per subject
  in `minion/vocabulary/` (`macos`, `browsers`, `office`, `media`, `dev`,
  `apps`, `sites`), embedded with `include_str!` so Minion works with
  nothing else on disk, then every `*.toml` in
  `~/Library/Application Support/Minion/vocabulary/`, then `config.toml` —
  later wins, matched by `name`. A `[[commands]]` entry says what it does
  with exactly one of `keys`, `action` (a **closed** list of named Rust
  actions), `text` or `url`; `bundles = [...]` makes it contextual. No
  scripts from a file on purpose: a pack may have been downloaded.
  `local.example.toml` holds the three machine-specific apps (Orca, Teams,
  ChatGPT) and is documented, not loaded. Not done: a community repo and an
  «Actualizar vocabulario» menu item.
- `commands.rs` — deciding, and everything that is a code path rather than
  a table: `DEFAULT_WAKE_WORDS`, `NUMBERED`, `BROWSERS`, dictation, undo,
  repeat, chain splitting, questions, shortcuts, macros, search; `decide_in()`
  order: dictation → mode/undo → contextual → global commands → questions →
  numbered → music → user aliases → shortcuts → websites → apps. Wake word is
  matched fuzzily (≤1 edit, 4 shared leading letters; 2 edits let "minuto"
  and "mínimo" through, so listed two-edit forms as explicit aliases
  instead) and a wake word split in two ("Mini on") is rejoined before
  judging. App aliases match whole words only (`near_alias`/`run_together`,
  never a substring search — that read "gmail" as containing "mail" and
  opened Mail), with a bounded one-edit fuzzy path scored 0.8, below a plain
  or run-together mention. English app names ("Chrome", "Safari") are also
  matched **phonetically**: `text::phonetic()` reduces both the alias and
  what was heard to how they'd sound said in Spanish ("cromo"), so "crum"
  and "cromo" match "Chrome" without listing every mangling by hand.
- `session.rs` — the per-utterance state machine: dictation mode, undo,
  "otra vez", chain splitting, the conversation window (no wake word needed
  for a few seconds after a command), and active learning — a phrase heard
  close to a known one but not close enough sets a `Pending` question
  ("¿Querías decir «abrir Safari»?") with a 6-second deadline, answered with
  «sí»/«no» and no wake word; a yes is written to `config.toml` as an alias.
  Pure and unit-tested; `main.rs` matches on `Outcome` and does the IO.
- `dictation.rs` — what continuous dictation does to words before they are
  typed: spoken punctuation, capitalisation, personal vocabulary
  (`[[dictation_words]]`), numbers up to 999,999. `Transformer` is pure and
  holds only what one session needs between chunks; `main.rs` owns one for
  the life of a session and feeds it every typed chunk. Edits
  (`EditIntent::DeleteLastWord/DeleteLastPhrase/Replace`) are computed from
  what has actually reached the keyboard so far, never from what was
  spoken, since spoken punctuation and personal vocabulary can change the
  length.
- `timers.rs` — Spanish numbers, spoken durations ("cinco minutos") and
  clock times ("a las ocho y media"), plus the pending-timers list itself
  (a `static Mutex<Vec<Pending>>`). Kept apart from `answers.rs` on purpose:
  this is parsing and bookkeeping, not vocabulary. An alarm resolves
  am/pm by picking whichever twelve-hour reading is soonest from now.
- `reminders.rs` — pure Spanish date parsing, then Recordatorios and
  Calendario via `osascript`. Nested inside `answers.rs` (a `#[path]` mod),
  not declared in `main.rs`. Parsing takes `now` as an argument so tests do
  not depend on the wall clock; clock-time phrases go through
  `timers::parse_alarm` rather than a second parser for the same grammar.
  Every `osascript` call is bounded to 5 seconds so a stuck permission
  dialog cannot hang the answer.
- `notify.rs` — Notification Center banners via
  `osascript -e 'display notification'` rather than `UNUserNotificationCenter`,
  which needs a notification entitlement and, in practice, a distribution
  mechanism a locally-signed dev build doesn't have. The trade-off: the
  banner is attributed to "Script Editor"/"osascript", not "Minion". Best
  effort — a failure here is silent, same as `actions::show_message`.
- `api.rs` — `minion run "…"` / `minion say "…"`, a tiny file-based local
  API for a launcher (Raycast, Atajos) to drive the already-running copy.
  No IPC: it leaves a request file next to `config.toml`, and the running
  copy picks it up on the same once-a-second timer that already checks for
  a duplicate launch. `run` goes through `commands::decide_in` with the
  wake word prefixed and is logged as `api` rather than `ran`. Runs on the
  menu bar's thread, not the listening thread, so it cannot continue a
  dictation session or be undone with "deshaz"; the speaker check is
  skipped deliberately — a shortcut typed from the keyboard has already
  proven who is there.
- `shortcuts.rs` — Apple Shortcuts, run by name only; Minion never
  inspects or edits one. The installed list (`shortcuts list`) is read once
  at startup and cached — that command is slow enough to notice on an
  always-on microphone — and refreshed only when a spoken name matches
  nothing, in case one was added since.
- `system.rs` — AppleScript for what a keystroke cannot reach: window
  geometry, system toggles, clipboard. Every script here is a plain
  `&str`, no runtime interpolation, because `vocabulary::NAMED_ACTIONS` is
  a `const` array. Left out on purpose: Bluetooth (needs third-party
  `blueutil`) and Do Not Disturb (no stable accessibility path since it
  moved into Control Centre). Brightness goes through `key code 144/145`
  rather than a real `NX_KEYTYPE_BRIGHTNESS` event. Wi-Fi assumes the
  interface is `en0`.
- `microphone.rs` — who else is capturing the microphone right now, via
  CoreAudio's per-process "is this recording" property rather than "which
  app is in front" (a guess that got calls in background windows wrong and
  foreground chat windows wrong). Never called from the audio callback,
  since a CoreAudio property read can block; polled from the idle tick
  (~250 ms) and cached for a second.
- `text.rs` — normalise, keyword F1 similarity, `words_match` (≤1 edit, or
  a shared stem ≥6 letters covering ¾ of the shorter word) with a real
  `strict` mode — a single-keyword command gets no edit-distance slack at
  all, so "cortar" cannot fire as "contar" — and `edits_between`.
- `spanish.rs` — filler words, verb canonicalisation (explicit table, no
  stemmer on purpose). `text::keywords()` canonicalises verbs *before*
  dropping fillers: "para" is both a filler and the imperative of "parar",
  and dropping first turned "para la música" into just `[musica]`, which
  paused Spotify instead of opening it.
- `speaker.rs` — ECAPA-TDNN embeddings, cosine similarity, one file per
  enrolled voice in `voices/` (the best match above the threshold decides
  who spoke; every voice may do everything, there are no tiers), threshold
  **0.32** (measured on real mic audio: worst 0.38, avg 0.57; 0.45 rejected
  the owner). Verifies short clips by tiling them up to a working length
  instead of waving them through unchecked, and embeds only the speech
  slice of the utterance (via `Utterance.speech_start/end`), not the
  preroll and silence around it. `fbank.rs` is the Kaldi-compatible mel
  frontend.
- `answers.rs` / `speech.rs` — spoken answers: time, date, battery, volume,
  help, timers/alarms, read-aloud (selection or clipboard, one sentence at
  a time so it can actually be interrupted), reminders and calendar
  (delegating the parsing to `reminders.rs`), now-playing, open apps, disk
  space, connectivity — via the system synthesiser, killable mid-sentence.
- `preferences.rs` — AppKit window; controls are **polled** by an NSTimer in
  `NSRunLoopCommonModes` (not Objective-C targets — documented at the top).
- `hotkey.rs` — CGEventTap on its own thread (an `NSEvent` global monitor on
  the main loop broke the menu). Default ⌥Space pauses/resumes. Re-enables
  the tap on `TapDisabledByTimeout`/`TapDisabledByUserInput` — macOS turns a
  slow tap off and it used to never come back, so ⌥Space would silently
  stop working until the next restart.
- `actions.rs` — key codes (positional! Spanish ISO layout breaks `cmd-[`
  etc.), open/quit apps, type text, shortcut parsing.
- `learn.rs` / `enroll.rs` — turn `unknown` log lines into aliases; record
  the voice profile. `journal.rs` rotates while running (not just at
  startup) and mirrors to stdout only when stdout is a TTY (launchd
  redirects it to a second, unrotated log otherwise). `config.rs` edits
  are pure functions (`with_option(contents, key, value)`) with
  `toml_string` escaping quotes and backslashes before they reach the
  file. `models.rs` pins each download to a repository revision and checks
  its SHA-256 before keeping it. `icon.rs`, `startup.rs` are what their
  names say.
- `assets/*.svg` — the face. Asleep keeps the **same smile**, only the eye
  closes. Dictating is the head filled in with goggle, pupil and smile cut
  out through an SVG mask (resvg renders masks; a pixel test guards it).

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
- `fatal()` must *wait* for its dialog (`show_message_and_wait`), not queue
  it and exit — two branches built on `show_message` independently, one
  making it async, the other calling `exit` right after, so the process
  was gone before the `NSAlert` could appear.
- launchd kills a job's whole process group when its main process exits,
  so a restart cannot be "spawn `open -n`, then exit": the child dies too.
  «Reiniciar» asks launchd (`kickstart -k`) when `XPC_SERVICE_NAME` says
  launchd started us, and only falls back to a `setsid`-detached shell.
- Never write to the journal from inside the CGEventTap callback: it opens
  a file and takes a lock, which is exactly the kind of slowness that gets
  macOS to disable the tap in the first place. Log outside it, on the run
  loop, between callbacks.
- The Accessibility-pane prompt must fire once per **boot**, not once per
  process start: launchd's `ThrottleInterval` restarts a crash-looping
  Minion every 10–30 s, and each restart used to reopen System Settings.
  `kern.boottime`, stashed in Application Support, is the marker.
- A keep-both merge can leave a struct's fields sitting inside a
  neighbouring enum instead of the struct — it compiles as long as both
  are in the same file's diff hunk, and only fails once something tries to
  read the field. Happened for real in commit `24f2581`: two feature
  branches both touched the end of `Config`, and the merge left `macros`
  and `search_engine` declared inside the `ListenMode` enum. Check `Config`
  itself, not just that the crate builds, after resolving a merge that
  touches `config.rs`.
- A stray top-level key after `[audio]` (or any `[table]` header) in
  `config.toml` invalidates the **whole file**, not just that key —
  `deny_unknown_fields` plus TOML's own key-belongs-to-preceding-header
  rule. `config::load()` falls back to defaults and only logs it; that
  silent fallback is exactly why the startup dialog
  (`config::problem()` in `main.rs`) exists — told once, on screen, in
  Spanish, instead of a Minion that has quietly forgotten every setting.
- AppleScript's own `empty trash` command skips Finder's confirmation
  dialog entirely — convenient, and exactly what "no phrase deletes
  anything on its own" forbids. `system::EMPTY_TRASH` never uses it:
  it activates Finder and sends ⇧⌘⌫ instead, so the same dialog a person
  emptying the Trash by hand would see still appears.

## Open items

- `blank` lines: voice matched, Parakeet returned no text. Seen a few times
  per round; if it grows, lengthen the preroll.
- Not planned unless asked: lowering
  the 405 MB idle floor.
