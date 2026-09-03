# Minion

Control your Mac by speaking Spanish. Always listening, entirely offline.

```
«minion, abre Chrome»          → Chrome comes forward
«minion, guarda esto»          → ⌘S
«mañana quedamos a las cinco»  → ignored
```

Lives in the menu bar as a small face — eye open while listening, eye shut
when paused. No Dock icon.

The menu holds one toggle (Pausar / Escuchar), an *Últimas órdenes* submenu
(the last five things heard, each with *Repetir*, *Crear alias…* and
*Olvidar alias* — the last two only where it applies), a *Registro* submenu
(*Aprender*, *Ver el registro*), *Ajustes…*, *Reiniciar*, *Ayuda*, and
Salir.

The icon is a template image drawn once as SVG, rendered for the menu bar
at run time and for the app at build time, so the two cannot drift apart.
macOS tints it from its alpha, black on a light bar and white on a dark one.
Besides listening/paused it has faces for thinking (between an utterance
ending and a slow decision, such as reloading the model), speaking (while
it talks back), and dictating (goggle, pupil and smile cut through a
mask), plus a blink for a command just carried out.

## Questions

Everything else in the vocabulary *does* something; these *return*
something, which is the whole reason for speaking aloud — the answer is the
point and there is nowhere else to put it.

```
«minion, ¿qué hora es?»            → «Las dos y veinte»
«minion, ¿qué día es hoy?»
«minion, ¿cuánta batería queda?»   → «68% y cargando»
«minion, ¿qué volumen tengo?»
«minion, ¿me oyes?»                → «Sí, te escucho»
«minion, ¿qué he dicho hoy?»       → «40 órdenes hoy, y 3 que no entendí»
«minion, ¿qué suena?»              → «Vértigo de Ela Minus»
«minion, ¿qué apps tengo abiertas?»
«minion, ¿cuánto espacio queda?»   → «212.4 GB libres»
«minion, ¿estoy conectado?»
«minion, ¿qué día de la semana es el 12?»
```

**Temporizadores y alarmas** — «pon un temporizador de cinco minutos»,
«avísame en diez minutos», «pon una alarma a las ocho y media». Numbers as
words (1–59, «y media», «y cuarto», «menos cuarto»). «¿cuánto queda?» and
«cancela el temporizador» work on whatever is pending. When one goes off:
a chime, a spoken line, and a Notification Center banner — the banner
regardless of `speak`, since the point of a timer is to be noticed even
from another room.

**Reading aloud** — «lee esto» / «lee la selección» copies the current
selection (⌘C, then puts the previous clipboard back), «lee el
portapapeles» reads whatever is already on it, «para de leer» stops
mid-sentence. Long text is read one sentence at a time so it can actually
be interrupted rather than only after the whole thing finishes.

**Recordatorios y calendario** — «recuérdame comprar pan», «recuérdame
llamar a Ana a las cinco», «recuérdame regar las plantas mañana a las diez»,
«recuérdame pagar el alquiler el viernes». Understands «mañana», «pasado
mañana», «esta tarde» (17:00) and «esta noche» (21:00) besides the usual
clock times; a reminder with no date and no time at all is created without
a due date. Confirms with «Te lo recordaré a las cinco» — or, for a plain
reminder, «Vale, recordado».

«¿qué tengo hoy?», «¿qué tengo mañana?» and «¿cuál es mi próxima reunión?»
read the first five events of the day (sorted, across every calendar) as a
spoken list: «A las diez, reunión con Ana. A las cuatro, dentista.» «añade
evento cena con Ana mañana a las diez», «crea un evento revisión el lunes a
las nueve y media» add a one-hour event the same way — to the first
calendar in the list, since Calendar.app has no scriptable "default
calendar" the way Reminders has a default list.

Both talk to Recordatorios and Calendario through `osascript`, which macOS
gates behind its own permission prompt the first time each is used —
**Recordatorios** the first time something is remembered, **Calendarios**
the first time an event is read or added. Grant both from *Ajustes del
Sistema → Privacidad y seguridad*; until then, a reminder fails silently
into the log and a calendar question answers «No puedo leer el calendario;
da permiso a Minion en Ajustes → Privacidad → Calendarios» (or the
Recordatorios equivalent for a reminder). Every call to `osascript` here is
bounded to 5 seconds, so a stuck permission dialog cannot hang Minion's
answer.

It stays quiet for commands. Opening Chrome is something you can see, and
announcing it would be noise arriving after the fact — the icon's blink
already says it was heard. Answers are short and plain: a reply heard forty
times a day should not be trying to entertain.

While speaking, Minion stops acting on what it hears and discards whatever
arrived meanwhile. It listens continuously, so its own voice comes straight
back in through the microphone.

Uses the system synthesiser and whichever Spanish voice macOS has, which
costs nothing to ship and is good enough to settle the harder question of
*when* to speak. Turn it off in preferences, or set `speak = false`.

## Escuchando

**Ventana de conversación** — for a few seconds after a command runs or a
question is answered, the next sentence needs no wake word:
«minion, abre Chrome» … «cierra la pestaña». `conversation_seconds`, 5 by
default; 0 turns it off so every sentence needs «minion» again.

**Empuñar para hablar** — with `listen_mode = "hold"`, Minion only listens
while the pause/resume shortcut (`resume_shortcut`, `ctrl-alt-m` by
default) is held down, and no wake word is needed while it is. Anything
other than `"hold"` behaves as the default, always-on `"always"`.

**Pausa automática** — locking the screen or letting the Mac sleep always
pauses Minion, no configuration needed. With
`pause_when_microphone_busy = true` (the default), it also pauses while
another process is actually recording from the microphone — a video call,
whatever the app — and resumes when that stops. The signal is CoreAudio's
own list of processes with an input stream open right now, not which
application is in front: Google Meet in a background browser tab counts,
Teams in the foreground with no call does not. Needs macOS 14 (Sonoma) or
later; on macOS 13 the option does nothing and says so once in the log.

## Ajustes

A native window, opened from the menu (called *Ajustes* — macOS 13+'s name
for Preferences). It covers what is worth changing without reading
documentation:

- sound on running a command
- whether what other people say is written to the log, or only counted
- starting at login
- **sensibilidad** — how sure Minion must be before acting, as words rather
  than a number: nobody wants to type 0.72, they want it to be less touchy
- **pausa que cierra una frase** — longer if it cuts you off while thinking
- **liberar memoria** — idle minutes before the model is released
- keeping recordings of what was heard, for working out why recognition
  behaves oddly — off by default, since it writes everything said nearby
- the pause/resume keyboard shortcut, captured by pressing it rather than
  typing its name; a modifier is required, Escape cancels the capture, and
  **Ninguno** turns the shortcut off entirely — default is `ctrl-alt-m`
  (not ⌥Space, which Alfred, Raycast and Spotlight commonly remap)
- **tu voz** — enrol or forget the voice profile Minion checks commands
  against; *Olvidar mi voz* deletes it after confirming, and Minion goes
  back to obeying whoever speaks
- **modo de escucha** — siempre, or only while the shortcut above is held
- **conversación** — how many seconds after a command the wake word can be
  skipped
- **dictado** — spoken punctuation and auto-capitalisation, each its own
  switch
- **pausa automática** — pause while another app is actually recording
- **avisos** — Notification Center banners for answers and timers
- **búsqueda** — which engine a bare «busca X» reaches (Google, YouTube,
  Wikipedia, Amazon)

The menu bar's tooltip mirrors this without opening anything: it says
whether Minion is listening or paused, and after each utterance shows the
last thing heard and what happened to it, shortened to one line.

The controls report by being read rather than by calling back. AppKit
delivers actions to an Objective-C target, which from Rust means declaring
a class — the most delicate part of the bridge, for a panel that changes at
human speed. The run loop timer that already repaints the menu bar reads
them a few times a second and writes through whatever moved. It also means
the window follows along when a setting changes elsewhere.

Everything else still lives in `config.toml`, which the window edits
without disturbing: the file keeps its comments, and settings the window
does not cover — wake words, extra applications, aliases, your own
commands — are only there.

## Why

Talon has excellent command recognition but its Conformer engine is
English-only, and no Spanish model exists. Handy understands Spanish
beautifully but is push-to-talk. Minion is the missing combination:
Spanish recognition with hands-free listening.

## Quick start

```bash
./install.sh    # builds, installs to /Applications, starts at login
```

The speech models (~670 MB) are downloaded on first run into Application
Support rather than carried inside the app, which is 29 MB. They never
change, and keeping them outside means reinstalling does not fetch them
again. `./download-model.sh` gets them ahead of time if you would rather
not wait on the first launch.

Each file is pinned to a specific commit of its Hugging Face repository —
not the mutable `resolve/main` — and checked against a known SHA-256
before being kept, so a truncated download or a swapped model is rejected
rather than silently accepted. `MINION_MODEL` points Minion at a model
folder somewhere else, for testing a different build.

Grant microphone access when asked. For commands that press keys (copy,
save, close tab) also grant Accessibility under System Settings → Privacy
& Security → Accessibility, then restart Minion.

Without it those commands fail invisibly: macOS accepts the key event and
discards it, so the log shows the command running while nothing happens on
screen. Minion checks at startup with `AXIsProcessTrusted`, says so in the
log, and opens the settings pane for you.

Two more permissions arrive later, each the first time it is actually
needed rather than at startup: **Recordatorios**, the first time something
is remembered, and **Calendarios**, the first time an event is read or
added. Both are macOS's own prompt for the `osascript` call underneath —
grant them from *Ajustes del Sistema → Privacidad y seguridad* the same
way as the others.

The permission is granted **per binary**, so rebuilding invalidates it.
Install to /Applications and grant it there, rather than granting it to a
copy in the project folder that you will replace on the next build. Keep
one copy only: two bundles with the same name make it impossible to tell
which one the switch in System Settings refers to.

If the permission gets into a confused state:

```bash
tccutil reset Accessibility com.studiolxd.minion
open /Applications/Minion.app     # asks again
```

Installing as an app matters for more than tidiness: macOS attributes
permissions to whichever binary asks for them. Run from a terminal and the
microphone permission belongs to the terminal, which then grants it to
every script you run there. Bundled, it is Minion's alone.

`./uninstall.sh` removes the app and the launch agent, but leaves your
data — voice profile, config, recordings, logs — behind, the same way
quitting an app does not erase its documents. `./uninstall.sh --purge`
deletes that too, after a typed confirmation.

Other ways to reach Minion from a terminal:

```bash
minion --help                       # this, in Spanish
minion enroll                       # record a voice profile without the window
minion export-icon <directory>      # write the menu-bar face as a .iconset
minion learn [--apply]              # turn `unknown` log lines into aliases
minion run "abre chrome"            # runs an order, as if you had said it
minion say "hola"                   # speaks (or notifies) some text
minion status                       # whether the running copy is listening
```

## Local API

`minion run` and `minion say` leave a small request file for the copy that
is already running and exit immediately — they do nothing on their own,
and nothing if Minion is not running. The running copy picks the request up
on the same once-a-second timer that already checks for a duplicate
launch, runs it with the wake word prefixed (`run`) or speaks it directly
(`say`), and logs it as `api` rather than `ran`, so the two sources stay
apart in `minion.log`.

This is meant for a keyboard shortcut or a launcher, not for scripting a
sequence of steps: a `run` command is not part of a dictation session and
cannot be undone with "deshaz" the way something actually heard can, and
the speaker check does not apply to it — running it from a shortcut has
already proven who is at the keyboard.

**Raycast** — a Script Command:

```bash
#!/bin/bash
# @raycast.title Minion: cierra la pestaña
# @raycast.mode silent
/Applications/Minion.app/Contents/MacOS/minion run "cierra la pestaña"
```

**Atajos (Shortcuts.app)** — a "Run Shell Script" action calling the same
binary and arguments, bound to whatever trigger you like.

## How it works

```
microphone (48 kHz)
    │  downmix, low-pass filter, decimate
16 kHz mono
    │  energy-based speech detection
one utterance
    │  Parakeet TDT 0.6b v3, ONNX int8, on CPU
Spanish text
    │  does it start with "minion"?
command executed
```

Everything runs locally. Nothing is sent anywhere.

## Speaking to it

Every command opens with the wake word — **minion**, by default. Only at
the start, so "le dije al minion que abriera Chrome" does nothing. It is
matched loosely: a recogniser slip like "minial" or "minio" still opens a
command, and "mini on" — the wake word split into two by the recogniser —
is rejoined before being judged. Configurable in `config.toml`.

Phrasing is forgiving by design. Filler words are dropped and verbs are
reduced to one form, so a single entry in the table covers the ways people
actually speak:

| These all work | |
|---|---|
| «minion, cierra la ventana» | the natural phrasing |
| «minion, cerrar ventana» | infinitive, no article |
| «minion, cierra ventana» | clipped |
| «minion, cierra la ventana por favor» | with politeness |

### Applications

«minion, **abre** Chrome» — and also *ábreme, ve a, vete a, cambia a,
pon, ponme, dame, trae, tráeme, muestra, enfoca, saca, lanza*. Or just name
it: «minion, Spotify».

To close one: «minion, **cierra** Safari» — also *sal de, termina, mata*.
It is a polite quit, so an app with unsaved work still puts up its own save
dialog.

The verb decides what happens to the application named. "cierra Safari"
quits it; "cierra la pestaña" closes a tab even though no app is named;
"minimiza la ventana" does neither. Naming an app is not on its own an
instruction to open it.

Chrome · Safari · Firefox · Terminal · VS Code · Finder · Mail · Notas ·
Calendario · Ajustes · Vista Previa · Monitor de Actividad · Word · Excel ·
PowerPoint · Pages · Numbers · Keynote · Spotify · Music · WhatsApp ·
Telegram · Discord · Figma · Obsidian · Claude

The list is not compiled in — see **Adding commands and applications**
below. «Qué puedo decirle» in the menu always shows what is really loaded.

An English name is also matched **by sound**, not just by one-letter
slips: "Chrome" said in Spanish comes out closer to "cromo", and Minion
reduces both the alias and what it heard to how they'd sound spoken that
way before comparing them, rather than needing every mangling ("grum",
"crum", "crumb"…) listed by hand.

If what was heard is close to a known phrase but not quite an exact match,
Minion asks about it out loud instead of staying silent: «¿Querías decir
«abrir Safari»?», answered with «sí» or «no» within six seconds, no wake
word needed. A «sí» is remembered as an alias, so the same near-miss works
next time. Turn this off with `ask_before_learning = false` — Minion then
only writes the near-miss to the log, for `minion learn` to pick up later.

### Web addresses

«minion, **ve a** google.com» — spelled out, or spoken with the dot as a
word ("github punto com"). Well-known sites work by name alone: google,
youtube, gmail, github, wikipedia, drive, maps, calendar, linkedin, amazon,
netflix.

An application always wins over a site: "abre Chrome" opens the browser,
not a search for it.

**The page opens where you are working.** Ask for a site while Chrome is in
front and it opens in Chrome, not in whichever browser the system considers
default. Ask from anywhere else and the default applies.

### Searching

«minion, **busca** gatos **en** Google» — also YouTube, Wikipedia, Amazon,
or "el Finder" for the folder already open in front. Say it with nothing
after "en" and it goes to the default engine — "google" unless
`search_engine` in `config.toml` says otherwise.

### Apple Shortcuts

«minion, **ejecuta el atajo** Modo Trabajo» — also *lanza el atajo* or just
*atajo Modo Trabajo*. Runs an installed Shortcut by name, matched the same
tolerant way an application's is. Minion starts it and moves on without
waiting, since a shortcut can take anywhere from instant to tens of seconds.

### Macros

A macro is a name for several phrases, said one after another. Define one
of your own in `config.toml` — see **Configuration** below — and Minion
runs each step as if it had been said, 300 ms apart, stopping at the first
one that is refused. A macro cannot call another macro.

### Dictating text

«minion, **escribe** hola qué tal» types the words that follow —
also *anota, apunta, dicta*.

Everything after the verb is content, never a command: "minion escribe
cierra la ventana" types the phrase instead of closing anything. Accents
and capitals survive, because the text comes from the original transcript
rather than the normalised form used for matching, and it is sent as a
Unicode string rather than as keystrokes, so ñ and á do not depend on the
keyboard layout.

### Dictado

"minion, **empieza a dictar**" (also *modo dictado*) opens a longer
session: everything said afterwards is typed, one chunk at a time, until
"minion, **deja de dictar**" (also *fin del dictado*). Three things happen
to the words on the way to the keyboard:

**Spoken punctuation.** Say the sign instead of typing it:

| Say | Types | Say | Types |
|---|---|---|---|
| coma | , | dos puntos | : |
| punto | . | punto y coma | ; |
| punto y aparte | . and a new paragraph | punto y seguido | . |
| nueva línea / salto de línea | a line break | puntos suspensivos | … |
| abre/cierra interrogación | ¿ / ? | abre/cierra exclamación | ¡ / ! |
| abre/cierra paréntesis | ( / ) | comillas | " (opens, then closes) |
| guion | - | arroba | @ |
| almohadilla | # | barra | / |

Spacing follows Spanish typography on its own: no space before `, . ; : ? !
)`, one after; nothing after an opening `¿ ¡ (` or an opening quote. To
type one of these words instead of its sign, say "**literal**" first —
"literal coma" types the word *coma*. Turn all of it off with
`spoken_punctuation = false`.

**Capitalisation.** The first word of a dictation, and whatever follows a
`. ? !`, gets a capital automatically. "**mayúscula**" capitalises just the
next word; "**en mayúsculas** X" shouts the rest of the chunk. Off with
`auto_capitalise = false`.

**Personal vocabulary.** For names the recogniser cannot spell — the log
showed «Mir Ángel Sufire García» for a real name — `config.toml` can say
what to type instead of what was actually heard:

```toml
[[dictation_words]]
heard = "mir angel sufire"
written = "Miguel Ángel Subir"
```

Matching ignores accents and case, checks the longest entry first, and
runs before punctuation, so a name mid-sentence still gets its signs
around it. Whole numbers work the same way without any configuration —
"treinta y cinco" or "número cuarenta y dos" type as digits — for figures
up to 999 999.

### The same words, read where you are

A phrase can mean the right thing in each place instead of needing a
different name per application:

| Say | Normally | In Finder | In Terminal |
|---|---|---|---|
| borra esto | ⌫ | to the Trash | ⌫ |
| cancela | Escape | Escape | ⌃C |
| sube del todo | scroll to top | enclosing folder | previous command |

### Only where they exist

Other phrases have no global meaning at all, and only work in one place:

| In | Say |
|---|---|
| Terminal | limpia la pantalla · principio de línea · final de línea |
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

| Music (Spotify) | Minion itself |
|---|---|
| pon la música · para la música | deja de escuchar |
| siguiente canción · canción anterior | (resume from the menu bar) |

Naming something searches for it: «minion, pon la canción Vértigo» —
also *el disco, el grupo, el artista, el tema*. It opens the results in
Spotify rather than playing straight away; starting a specific track needs
the Web API and an OAuth token, which is a different project.

A known command always wins over a title, so "pon la canción anterior"
goes back one rather than searching for a song called "anterior".

## The log

`~/Library/Logs/minion.log` records every utterance it heard and what it
did with it, whichever way the app was started:

```
21:24:02  Listening. Say: «minion, abre Chrome»
21:24:31  ran      «Minion, abre Chrome.»  ->  abrir Chrome  [96% · 1.8s audio · 240 ms]
21:25:04  heard    2.4s of speech, not addressed to me
21:25:19  unknown  «Minion, haz un pino.»  ->  not understood
```

This is the tool for tuning. `unknown` lines show what the recogniser
really produces from your voice, which is what should go into the alias
list. Frequent `heard` lines when nobody is speaking mean
`speech_threshold` is too low.

**Speech not addressed to Minion is counted, not transcribed.** With the
microphone always on, everything said nearby passes through the recogniser,
and writing other people's conversations to disk is not something anyone
asked for. Set `log_ignored_speech = true` while tuning, when seeing the
exact wording is the point.

It rotates at 5 MB, keeping one previous copy.

## Adding commands and applications

The vocabulary is TOML, not Rust. It ships as one file per subject in
`minion/vocabulary/`:

```
macos.toml      Apple's own apps, clipboard, windows, tabs, screenshots, sound
browsers.toml   Chrome, Safari, Firefox and what those commands mean inside one
office.toml     Word, Excel, PowerPoint, Pages, Numbers, Keynote
media.toml      Spotify, Music, and the transport controls
dev.toml        Terminal, VS Code, and the terminal-only commands
apps.toml       chat, design, notes — everything else opened by name
sites.toml      pages you can name without their domain
```

Those are built into the binary, so Minion works with nothing else on
disk. To add your own without rebuilding, **drop a `.toml` file in
`~/Library/Application Support/Minion/vocabulary/`** and restart. Anything
there is read after the built-in files, and `config.toml` is read after
that, so a later entry with the same `name` replaces the earlier one:

```toml
category = "Trabajo"

[[apps]]
name = "Notion"
bundle_id = "notion.id"
aliases = ["notion", "nocion"]

[[commands]]
name = "compilar"
phrases = ["compila el proyecto", "compila"]
keys = "cmd-shift-b"
bundles = ["com.microsoft.VSCode"]   # optional: only inside these apps

[[sites]]
name = "intranet"
url = "https://intranet.example.com"
```

A command says what it does with exactly one of `keys` (a shortcut),
`action` (one of Minion's own: `volume:up`, `volume:down`, `volume:mute`,
`volume:unmute`, `music:play`, `music:pause`, `music:next`,
`music:previous`, `minion:sleep`, `window:left`, `window:right`,
`window:maximize`, `window:other-screen`, `system:brightness-up`,
`system:brightness-down`, `system:dark-mode`, `system:wifi-on`,
`system:wifi-off`, `system:empty-trash`, `system:sleep-display`,
`system:screenshot-window`, `clipboard:clear`, `browser:copy-url`,
`browser:duplicate-tab-safari`, `browser:duplicate-tab-chrome`), `text`
(type this) or `url` (open this). There is deliberately no way to run a
script from a vocabulary file: it is data that may have been downloaded,
and data that runs is not data.

The schema is documented in full at the top of `vocabulary/macos.toml`. A
file that does not parse is reported in the log and skipped, and so is a
single bad entry — one typo costs that line, not the rest.
`vocabulary/local.example.toml` holds the three applications that only
exist on the machine Minion was written on; copy it if you have them.

Not done yet: a separate community repository of vocabulary packs, and an
«Actualizar vocabulario» item in the menu to fetch them. For now, packs are
files you put in that directory yourself.

## Configuration

Copy `config.example.toml` to
`~/Library/Application Support/Minion/config.toml`. Everything in it is
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

The file uses `deny_unknown_fields`: one misplaced or misspelled key
invalidates the **whole file**, not just that key — a top-level setting
must come before the first `[table]` header, such as `[audio]`, or TOML
reads it as belonging to that table instead. When that happens Minion
falls back to every default, says so in the log, and — since a config that
has quietly gone back to defaults is easy to miss — also puts up a dialog
on screen naming the problem, in Spanish, once at startup.

Beyond the wake words, threshold and audio numbers above, the keys this
round of work added, with their defaults:

| Key | Default | What it does |
|---|---|---|
| `conversation_seconds` | `5` | seconds after a command the wake word can be skipped; `0` disables it |
| `listen_mode` | `"always"` | `"hold"` for push-to-talk on `resume_shortcut` |
| `pause_when_microphone_busy` | `true` | pause while another process is recording (macOS 14+) |
| `spoken_punctuation` | `true` | turn spoken signs into punctuation while dictating |
| `auto_capitalise` | `true` | capitalise sentence starts and «mayúscula» while dictating |
| `ask_before_learning` | `true` | ask «¿Querías decir…?» about a near-miss, instead of only logging it |
| `notifications` | `true` | Notification Center banners for answers, timers and blocked commands |
| `search_engine` | `"google"` | which engine a bare «busca X» reaches |
| `dictation_words` | *(none)* | `[[dictation_words]]`, names the recogniser reliably mangles |
| `macros` | *(none)* | `[[macros]]`, named sequences of phrases |

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
the test suite checks about a thousand phrases without touching the system.

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

## Learning from its own mistakes

Every phrase Minion failed to understand is in the log. `minion learn`
reads them back, works out what each was probably meant to be, and offers
to add it as an alias:

From the menu bar: **Registro → Aprender**. It opens a window with what it
found — a real window with a close button, not a dialog that blocks
everything until dismissed — and asks before changing anything.

From a terminal:

```bash
/Applications/Minion.app/Contents/MacOS/minion learn
```

```
Phrases that look like an existing command:

  «Minion retroceder página.»  ×2
      → atrás (100% similar)

No command resembles these — they may need a new one:

  «Minion reproduce la canción vértigo.»
```

`--apply` writes the first group into `config.toml` as aliases. The
second group is the useful half: it is the list of things the vocabulary
does not cover yet.

Phrases the vocabulary has since learned are skipped, so the report shows
what is still missing rather than everything that ever failed.

## Testing

```bash
cargo test
```

383 tests (1 ignored). The ones that matter most check that ordinary conversation is
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

- **A tighter memory floor.** 405 MB idle is what ONNX Runtime gives back.
- **Real VAD.** Energy cannot tell speech from a door slam, and background
  music keeps it triggering. Silero VAD is the next step.

- **Developer ID signing**, so the app can be shared with other machines.
  The ad-hoc signature is enough for this one.

- **A community vocabulary repository**, and an «Actualizar vocabulario»
  menu item to pull packs from it. The loader already reads whatever is in
  `~/Library/Application Support/Minion/vocabulary/`; nothing fetches it.
