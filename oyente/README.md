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
./download-model.sh          # ~640 MB, once
cargo build --release
./target/release/oyente
```

Grant microphone access when asked. For commands that press keys (copy,
save, close tab) also grant Accessibility under System Settings → Privacy
& Security. Without it those commands silently do nothing — the program
warns about this at startup.

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

Chrome · Safari · Terminal · Orca · Finder · Mail · Notas · Calendario ·
Spotify · WhatsApp · Telegram · Figma · Obsidian · Discord · Teams ·
VS Code · Claude · ChatGPT · Ajustes · Vista Previa · Monitor de Actividad

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

## Tuning

The four numbers governing speech detection live at the top of
`src/audio.rs`:

| Setting | Default | If it is wrong |
|---|---|---|
| `speech_threshold` | 0.015 | Too high: clips words. Too low: transcribes the fan |
| `silence_end_ms` | 700 | Too low: splits sentences mid-thought. Too high: sluggish |
| `min_speech_ms` | 300 | Filters out door slams and coughs |
| `max_utterance_ms` | 12000 | Safety cut against continuous noise |

`THRESHOLD` in `src/commands.rs` is the match confidence needed to act. It
errs high on purpose: with an always-on microphone, firing a command that
was never spoken is much worse than missing one.

## Design notes

**Deciding and acting are separate.** `commands::decide` is pure and
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

26 tests. The ones that matter most check that ordinary conversation is
ignored, that every declared phrase reaches its own command, and that no
two commands claim the same phrase.

## Not done yet

- **Real VAD.** Energy cannot tell speech from a door slam, and background
  music keeps it triggering. Silero VAD is the next step.
- **Dictation.** Oyente runs commands; it does not type text.
- **Per-application context**, so one phrase means different things
  depending on what is in front.
- **App bundle and signing**, so microphone permission is attributed to
  Oyente rather than to the terminal that launched it.
- **Launch at login** via a launchd agent.
