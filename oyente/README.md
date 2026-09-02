# Oyente

Control your Mac by speaking Spanish. Always listening, entirely offline.

```
option-free:  «ordenador, abre Chrome»      → Chrome comes forward
              «ordenador, guarda esto»      → ⌘S
              «mañana quedamos a las cinco» → ignored
```

Lives in the menu bar as 🎙. No Dock icon, no window.

## Why

Talon has excellent command recognition but its Conformer engine is
English-only, and no Spanish model exists. Handy understands Spanish
beautifully but is push-to-talk. Oyente is the missing combination:
Spanish recognition with hands-free listening.

## Quick start

```bash
./download-model.sh    # ~640 MB, once
./install.sh           # builds, installs to /Applications, starts at login
```

Or to run it without installing:

```bash
./download-model.sh
cargo build --release
./target/release/oyente
```

Grant microphone access when asked. For commands that press keys (copy,
save, close tab) also grant Accessibility under System Settings → Privacy
& Security → Accessibility, then restart Oyente.

Without it those commands fail invisibly: macOS accepts the key event and
discards it, so the log shows the command running while nothing happens on
screen. Oyente checks at startup with `AXIsProcessTrusted`, says so in the
log, and opens the settings pane for you.

The permission is granted **per binary**, so rebuilding invalidates it.
Install to /Applications and grant it there, rather than granting it to a
copy in the project folder that you will replace on the next build. Keep
one copy only: two bundles with the same name make it impossible to tell
which one the switch in System Settings refers to.

If the permission gets into a confused state:

```bash
tccutil reset Accessibility com.studiolxd.oyente
open /Applications/Oyente.app     # asks again
```

Installing as an app matters for more than tidiness: macOS attributes
permissions to whichever binary asks for them. Run from a terminal and the
microphone permission belongs to the terminal, which then grants it to
every script you run there. Bundled, it is Oyente's alone.

`./uninstall.sh` removes it.

## How it works

```
microphone (48 kHz)
    │  downmix and decimate
16 kHz mono
    │  energy-based speech detection
one utterance
    │  Parakeet TDT 0.6b v3, ONNX int8, on CPU
Spanish text
    │  does it start with "ordenador"?
command executed
```

Everything runs locally. Nothing is sent anywhere.

## Speaking to it

Every command opens with the wake word — **ordenador**. Only at the start,
so "le dije al ordenador que abriera Chrome" does nothing.

Phrasing is forgiving by design. Filler words are dropped and verbs are
reduced to one form, so a single entry in the table covers the ways people
actually speak:

| These all work | |
|---|---|
| «ordenador, cierra la ventana» | the natural phrasing |
| «ordenador, cerrar ventana» | infinitive, no article |
| «ordenador, cierra ventana» | clipped |
| «ordenador, cierra la ventana por favor» | with politeness |

### Applications

«ordenador, **abre** Chrome» — and also *ábreme, ve a, vete a, cambia a,
pon, ponme, dame, trae, tráeme, muestra, enfoca, saca, lanza*. Or just name
it: «ordenador, Spotify».

To close one: «ordenador, **cierra** Safari» — also *sal de, termina, mata*.
It is a polite quit, so an app with unsaved work still puts up its own save
dialog.

The verb decides what happens to the application named. "cierra Safari"
quits it; "cierra la pestaña" closes a tab even though no app is named;
"minimiza la ventana" does neither. Naming an app is not on its own an
instruction to open it.

Chrome · Safari · Terminal · Orca · Finder · Mail · Notas · Calendario ·
Spotify · WhatsApp · Telegram · Figma · Obsidian · Discord · Teams ·
VS Code · Claude · ChatGPT · Ajustes · Vista Previa · Monitor de Actividad

### Web addresses

«ordenador, **ve a** google.com» — spelled out, or spoken with the dot as a
word ("github punto com"). Well-known sites work by name alone: google,
youtube, gmail, github, wikipedia, drive, maps, calendar, linkedin, amazon,
netflix.

An application always wins over a site: "abre Chrome" opens the browser,
not a search for it.

### Inside particular applications

Some phrases only exist where they mean something, and beat the global
command of the same name:

| In | Say |
|---|---|
| Terminal | limpia la pantalla · cancela · principio de línea · final de línea |
| Chrome, Safari | abre los favoritos · abre el historial · ventana de incógnito |
| Finder | crea una carpeta · muestra la información |

### Commands

| Editing | Tabs and windows |
|---|---|
| copia esto · pega esto · corta esto | abre una pestaña nueva |
| guarda esto | cierra la pestaña · recupera la pestaña |
| deshaz el cambio · rehaz el cambio | pasa a la siguiente pestaña |
| selecciona todo · borra esto | cierra la ventana · abre una ventana nueva |
| busca en la página | minimiza · pon la pantalla completa · esconde la aplicación |

| Navigation | System |
|---|---|
| vuelve atrás · ve adelante | captura la pantalla · recorta la pantalla |
| recarga la página | abre Spotlight · bloquea la pantalla |
| sube del todo · baja del todo | sube el volumen · baja el volumen |
| ve a la barra de direcciones | quita el sonido · devuelve el sonido |

| Music (Spotify) | Oyente itself |
|---|---|
| pon la música · para la música | deja de escuchar |
| siguiente canción · canción anterior | (resume from the menu bar) |

## The log

`~/Library/Logs/oyente.log` records every utterance it heard and what it
did with it, whichever way the app was started:

```
21:24:02  Listening. Say: «ordenador, abre Chrome»
21:24:31  ran      «Ordenador, abre Chrome.»  ->  abrir Chrome  [96% · 1.8s audio · 240 ms]
21:25:04  heard    2.4s of speech, not addressed to me
21:25:19  unknown  «Ordenador, haz un pino.»  ->  not understood
```

This is the tool for tuning. `unknown` lines show what the recogniser
really produces from your voice, which is what should go into the alias
list. Frequent `heard` lines when nobody is speaking mean
`speech_threshold` is too low.

**Speech not addressed to Oyente is counted, not transcribed.** With the
microphone always on, everything said nearby passes through the recogniser,
and writing other people's conversations to disk is not something anyone
asked for. Set `log_ignored_speech = true` while tuning, when seeing the
exact wording is the point.

It rotates at 5 MB, keeping one previous copy.

## Configuration

Copy `config.example.toml` to
`~/Library/Application Support/Oyente/config.toml`. Everything in it is
optional, and it is read once at startup.

It covers the wake words, the confidence threshold, the speech detection
numbers, and extra applications — so adding your own apps or retuning the
listener needs no Rust toolchain:

```toml
threshold = 0.75

[audio]
silence_end_ms = 900

[[apps]]
name = "Notion"
bundle_id = "notion.id"
aliases = ["notion", "nocion"]
```

A typo in the file is reported at startup and then ignored; it will not
stop Oyente from running.

## Tuning

The defaults for speech detection, overridable in the config file:

| Setting | Default | If it is wrong |
|---|---|---|
| `speech_threshold` | 0.015 | Too high: clips words. Too low: transcribes the fan |
| `silence_end_ms` | 700 | Too low: splits sentences mid-thought. Too high: sluggish |
| `min_speech_ms` | 300 | Filters out door slams and coughs |
| `max_utterance_ms` | 12000 | Safety cut against continuous noise |

`threshold` is the match confidence needed to act. It errs high on purpose:
with an always-on microphone, firing a command that was never spoken is
much worse than missing one.

## Design notes

**Deciding and acting are separate.** `commands::decide_in` is pure and
returns a `Decision`; `commands::perform` carries it out. That is what
makes the vocabulary testable without applications opening for real —
the test suite checks all 970 phrases without touching the system.

**Key codes are positional, not character-based.** Code 8 is wherever `C`
sits on a US keyboard, which on a Spanish ISO layout is also `C`, so ⌘C
works on both. Symbols are the exception: `[`, `]` and `=` are elsewhere
entirely, and shortcuts using them fail silently. That is why navigation
uses ⌘← rather than ⌘[.

**Edit distance is capped at one.** Two edits turns *ventana* into
*pestana*, *copiar* into *cortar* and *deshacer* into *rehacer* — pairs of
commands that do very different things. Bigger recogniser mangles are
handled by listing the mangled form as an alias, which is explicit and
safe. "cromo" is in the table because that is what saying *Chrome* in
Spanish actually produces.

**`open -b` rather than activating a process.** It covers three cases at
once: app not running, running with no windows, and running with a window.
The middle one is common on macOS, where closing the last window does not
quit the app.

## Testing

```bash
cargo test
```

30 tests. The ones that matter most check that ordinary conversation is
ignored, that every declared phrase reaches its own command, and that no
two commands claim the same phrase.

They have already earned their keep: they caught "cortar" being executed as
"copiar", "cierra la ventana" running "cerrar pestaña", and "rehaz" landing
on "deshacer" — all from an edit-distance allowance that was one step too
generous.

## Signing

`build-app.sh` signs with an Apple Development certificate when one is in
the keychain, and falls back to ad-hoc otherwise. The difference matters
more than it looks: an ad-hoc signature ties the Accessibility grant to the
exact bytes of the binary, so **every rebuild silently revokes it** and the
app goes back to being ignored by the window server with no error anywhere.
A real certificate ties the grant to the team and bundle id, which survive
rebuilding.

## Not done yet

- **Real VAD.** Energy cannot tell speech from a door slam, and background
  music keeps it triggering. Silero VAD is the next step.
- **Dictation.** Oyente runs commands; it does not type text.

- **Custom key commands in the config file**, not just applications.
- **Developer ID signing**, so the app can be shared with other machines.
  The ad-hoc signature is enough for this one.
