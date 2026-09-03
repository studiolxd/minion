# Feasibility studies — September 2026

Three questions asked of the code as it stands at commit `3ebc74c`
(562 tests, 2 ignored, all passing; `parakeet-rs 0.3.7`, `ort 2.0.0-rc.13`).

This is a study. No file under `minion/src` was changed to produce it.

Every external claim carries its source. Claims about Minion carry a
`file:line`. Nothing here was measured on real audio; where a measurement
would be needed before acting, that is said explicitly.

---

## 1. Other languages

### 1.1 What the recogniser can already do

`nvidia/parakeet-tdt-0.6b-v3` is multilingual: **25 European languages** —
Bulgarian, Croatian, Czech, Danish, Dutch, English, Estonian, Finnish,
French, German, Greek, Hungarian, Italian, Latvian, Lithuanian, Maltese,
Polish, Portuguese, Romanian, Slovak, Slovenian, Spanish, Swedish, Russian,
Ukrainian. The card states the model "automatically detects the language of
the audio and transcribes it without requiring additional prompting".
Source: <https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3>

**Catalan is not on that list.** Neither is Basque or Galician. Whatever is
done to the Rust, a Catalan Minion is blocked on a Catalan ASR model, not on
Minion's architecture.

The ONNX export in use keeps the multilingual vocabulary. Verified directly
against the copy this machine has downloaded
(`~/Library/Application Support/Minion/model/vocab.txt`, the file
`models.rs` fetches from `istupakov/parakeet-tdt-0.6b-v3-onnx` at revision
`8f23f0c0`):

* 8 193 tokens;
* 1 147 of them contain Cyrillic characters, 321 contain Spanish accented
  vowels or `ñ`, 74 contain Romanian-style diacritics;
* the special tokens are `<unk>`, `<|nospeech|>`, `<pad>`, `<|endoftext|>`,
  `<|startoftranscript|>`.

A Spanish-only export would not carry 1 147 Cyrillic tokens. The int8
quantisation (`encoder-model.int8.onnx`, `decoder_joint-model.int8.onnx` —
`models.rs:52-83`) touches weights, not the tokenizer, so nothing was lost
there either.
Source: <https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx>

**The catch.** `parakeet-rs 0.3.7` exposes *no* way to pin the language for
this model. Checked in the vendored crate source: `Nemotron` has
`set_target_lang("es-ES")` and `CohereASR::transcribe_audio` takes an ISO
639-1 code, but `ParakeetTDT` — the type `main.rs:59` imports and
`main.rs:979` calls — has neither. Every utterance goes through
auto-detection.

Two consequences, and they pull in opposite directions:

* **Adding a language costs nothing on the ASR side.** No second model, no
  download, no configuration. The recogniser already understands Portuguese.
* **Minion cannot ask for Spanish either.** The log line "minimized the
  ventana" recorded in `CLAUDE.md` is the auto-detector switching language
  mid-utterance. That is a documented property of the model, not a defect in
  `text::similarity`. Worth writing down so nobody spends another round
  tuning the matcher against it.

### 1.2 Inventory: every Spanish-specific piece

| What | Where | Shape today | Kind |
|---|---|---|---|
| Filler words (48) | `spanish.rs:12` `FILLER` | `const &[&str]` | data |
| Verb canonicalisation (~110 forms → ~60 stems) | `spanish.rs:26` `VERBS` | `const` pairs | data (needs a native speaker) |
| Accent folding | `text.rs:14` `normalise` | `match` arms | code **and** data |
| Phonetic reduction (Spanish orthography) | `text.rs:73` `phonetic` | ~20 rules in Rust | **code** |
| Wake words | `commands.rs:36` `DEFAULT_WAKE_WORDS` | `const`, already config-overridable | data |
| App / quit / music / dictation verb sets, TLDs, Finder words, chain joiners | `commands.rs:169,175,178,185,192,352,369` | `const` | data |
| Number words 1–9 with ordinals | `commands.rs:373` `NUMBERS` | `const` | data |
| Numbered-command subjects (`"pestana"`) | `commands.rs:405` `NUMBERED` | `const` + `fn` | code and data |
| Search engines | `commands.rs:344` | `const` | mostly data |
| Question phrasings (19 questions, ~55 phrases) | `answers.rs:72` `ASKED` | `const` | data |
| Weekday and month names | `answers.rs:354,356`, `reminders.rs:46` | `const` | data |
| Spoken date grammar (`"{day}, {n} de {month}"`) | `answers.rs:366` | inline format string | **code** |
| Durations and clock grammar (`y media`, `menos cuarto`, `a las`) | `timers.rs:92-135` | `match` + arithmetic | **code** |
| Number words 0–59 | `timers.rs:18-58` | `match` | data |
| Number words to 999 999 | `dictation.rs:510,539,562` | `const` | data |
| Spoken punctuation (~25 phrases) and Spanish typography (`¿` `¡` open) | `dictation.rs:314-386` | `match` arms + atom kinds | code and data |
| Yes / no answers | `session.rs:216-217` | `const` | data |
| Ordinal choice words | `session.rs:157-158` | `const` | data |
| TTS voice selection (`es_ES` filter, Mónica/Paulina) | `speech.rs:23,46` | `const` + filter | code and data |
| Vocabulary: 151 phrases, 30 alias lists, 29 apps, 85 commands, 12 sites, 4 destinations | `vocabulary/*.toml` | TOML | **already data** |
| AI system prompts and the command-matching prompt | `ai/mod.rs:80,516,522` | `const` | data |
| UI, log, dialog and catalogue strings | 27 files | inline literals | data, and the bulk of the work |

For scale on the last row: **355 string literals across 27 files contain an
accented character or `¿`/`¡`** (`commands.rs` 103, `main.rs` 35,
`answers.rs` 35, `session.rs` 21, `ai/mod.rs` 19, `preferences.rs` 17, …).
The true number of Spanish user-facing strings is higher, because plenty of
them have no accent in them at all.

Language-free already, and staying that way: `audio.rs`, `fbank.rs`,
`silero.rs`, `speaker.rs`, `system.rs`, `models.rs`, `journal.rs`,
`icon.rs`, `hotkey.rs`, `startup.rs`, `corpus.rs`, `timers.rs`'s pending
list (as opposed to its parser).

### 1.3 Proposed architecture

Not "a `Language` trait" **or** "a `lang/es.toml`" — both, with the line
drawn at a specific place: **a table if it is a list of words, a trait
method if it does arithmetic or rewrites letters.**

**Layer A — `minion/lang/es.toml`,** embedded with `include_str!` exactly
the way `vocabulary/*.toml` already is (`vocabulary.rs:42`), so Minion still
works with nothing else on disk. It holds: fillers, the verb table, number
words, weekday and month names, question phrasings, yes/no words, ordinal
words, punctuation phrases, wake words, the app/quit/music/dictation verb
sets, chain joiners, TLDs, and the TTS voice preference. This is the great
majority of the inventory above and none of it is interesting to a compiler.

**Layer B — `trait Language`,** for the four things a table cannot hold:

```rust
trait Language {
    fn fold(&self, c: char) -> char;              // text::normalise
    fn phonetic(&self, word: &str) -> String;     // text::phonetic
    fn parse_duration(&self, w: &[&str]) -> Option<(Duration, String)>;
    fn parse_alarm(&self, w: &[&str]) -> Option<(NaiveDateTime, String)>;
    fn spoken_date(&self, now: DateTime<Local>) -> String;
    fn min_stem(&self) -> usize { 6 }             // text::shares_stem
}
```

Each of these is genuinely per-language:

* `fold` — which diacritics are noise and which are phonemes differs.
  `ñ`→`n` is right for Spanish matching; `ș`→`s` is right for Romanian;
  `ã`→`a` throws away a Portuguese phoneme. A table of pairs would work,
  but the *decision* about what to fold is a language's, so it belongs
  behind the same door as the rest.
* `phonetic` — Spanish's is 20 rules about how a Spanish mouth says an
  English app name (`text.rs:52-79`). English needs no such thing (the app
  names are already English); Portuguese needs a different set. An
  implementation is allowed to return the word unchanged, which turns the
  phonetic path off cleanly.
* `parse_duration` / `parse_alarm` — "las ocho y media", "menos cuarto" is
  grammar with arithmetic in it (`timers.rs:109-135`), not a lookup.
* `min_stem` — 6 is calibrated on Spanish word lengths. German compounds
  and Finnish agglutination make a 6-letter shared prefix far too generous;
  this must be per-language or `shares_stem` becomes a source of wrong
  commands.

Set once at startup from a new **top-level** `language = "es"` in
`config.toml` — top-level, and therefore written **before `[audio]`**, per
the `deny_unknown_fields` trap documented in `CLAUDE.md`. A
`static LANGUAGE: OnceLock<&'static dyn Language>` mirrors how `commands.rs`
already holds `VOCABULARY`, `USER_ALIASES` and the rest.

**Layer C — UI strings,** a `strings/<code>.toml` keyed by identifier with
`es` as the fallback, so a missing key can never blank a dialog. This is the
biggest and dullest part and it can be done **last**: a second language
ships usable on A+B alone, with a Spanish menu bar. Doing C first would be
four days of work with nothing to show.

**What stays code, and why**

* `text::similarity`, `match_quality`, `shares_stem`, `edit_distance` — the
  grades (1.0 / 0.9 / 0.8 / 0.75) encode *how this recogniser fails*, and it
  fails by whole words in every language it speaks. Only `min_stem` and the
  fold move.
* `dictation::Transformer`'s atom kinds (`glued` / `opening` / `fused`) —
  universal. Which sign is `opening` is data: Spanish's `¿` and `¡` become
  ordinary rows, and a language with no opening marks simply has none.
* `commands::decide_in`'s order — universal.
* `session::interpret` — the state machine is language-free; only its word
  lists move.

### 1.4 How vocabulary packs carry per-language phrases

**Not `phrases.en = [...]` inside an entry.** Two reasons: `VocabularyFile`
is `deny_unknown_fields` (`vocabulary.rs:99`), so changing the shape of
`phrases` breaks every file that exists and every pack already published;
and it makes every entry three times longer for the ~100 % of users who
speak one language.

Instead: **one optional top-level `language` key per file, defaulting to
`"es"`**, and the loader skips files whose language is not the active one.
A pack becomes `browsers.es.toml` / `browsers.en.toml` — a filename
convention plus one key.

```toml
language = "en"          # absent means "es"; every file today keeps working
category = "Browsers"

[[apps]]
name = "Google Chrome"
bundle_id = "com.google.Chrome"
aliases = ["chrome", "google chrome"]
```

`bundle_id`, `keys`, `action` and `url` are language-free and get duplicated
between the two files. That duplication is cheap, greppable and diffable —
unlike a nested per-language table, which nobody can review. The merge rules
(match by `name`, later wins) are untouched. `packs.rs`'s manifest gains a
`language` field so a future «Actualizar vocabulario» offers only the packs
for the active language.

### 1.5 Effort

| Piece | Days | Notes |
|---|---:|---|
| Framework: layers A + B, `es.toml` extracted, every test still green, no behaviour change | 6–8 | paid once; this is where the risk is |
| Layer C: 355+ literals, the catalogue, «Ayuda» | 4–6 | can be deferred |
| **English** | 2–3 | no accent folding; `phonetic` is the identity (the app names are already English, so the module that exists to catch "cromo" is a no-op); smallest verb table |
| **Portuguese** | 4–5 | in the 25; verb table comparable to Spanish; the real work is `fold` — `ã`/`õ` are phonemes, not decoration; numbers and clock grammar adapt rather than get written |
| **Catalan** | — | **blocked**: not one of Parakeet v3's 25 languages. The Rust would be ~3–4 days (closest relative to Spanish), but there is no model to feed it |

Total to a bilingual Spanish/English Minion: **12–17 days**.

### 1.6 First three steps

1. **Extract `lang/es.toml` with zero behaviour change.** Move `FILLER`,
   `VERBS`, `NUMBERS`, the weekday/month names, `ASKED`, `YES`/`NO`,
   `FIRST`/`SECOND` and the punctuation phrases into
   `minion/lang/es.toml`, load it with `include_str!`, and keep all 562
   tests passing plus `tests/corpus.rs`'s baseline. All of the project's
   risk is in this one step, and it is worth taking before anything else is
   promised.
2. **Introduce `trait Language` and `Spanish`,** which is today's code
   moved, not rewritten. Add the top-level `language` key to `Config`
   (before `[audio]`), and check `Config`'s own fields after the merge —
   see the `24f2581` note in `CLAUDE.md`.
3. **Add `language` to `VocabularyFile`** (optional, default `"es"`) and
   skip non-matching files in `vocabulary::load`. Ship a ten-entry English
   pack as proof, and confirm `minion corpus` still meets its Spanish
   baseline with the pack present and `language = "es"`.

### 1.7 One thing none of that fixes

Even complete, Minion would still transcribe through an auto-detecting model
with no way to say "expect Spanish" (§1.1). For a bilingual user the
language of a transcript is not deterministic, and `keywords()` would be
applied to whichever one came back.

Three options, in the order they should be considered:

* **(a) Accept it.** It is already the situation today and it is survivable.
* **(b) Score against every active language's tables and take the best.**
  Matching is microseconds; the tables are already in memory. Cheap, and it
  turns the auto-detector from a hazard into a non-event. This is the
  recommended follow-up.
* **(c) Move to `Nemotron` multilingual,** which parakeet-rs *does* let you
  pin (`set_target_lang("es-ES")`, 40 language-locales). Cost: different
  weights, different download, new SHA-256 pins in `models.rs`, and a fresh
  `minion corpus` baseline. Only worth it if (b) proves insufficient.
  Source: parakeet-rs README, <https://github.com/altunenes/parakeet-rs>

---

## 2. CoreML / Neural Engine

### 2.1 Support exists — and upstream tells you not to use it

`parakeet-rs 0.3.7` has a full CoreML path: a `coreml = ["ort/coreml"]`
feature, `ExecutionProvider::CoreML`, a `CoreMLComputeUnits` enum
(`All`, `CpuAndNeuralEngine`, `CpuAndGpu`, `CpuOnly`) and
`with_coreml_cache_dir`, which exists to avoid "~5s recompilation on each
session load". Enabling it in Minion would be one Cargo feature and one line
in `inference_config()` (`main.rs:437`).

The crate's own source, `src/execution.rs`, says not to:

> CoreML EP currently runs slower than CPU for Sortformer/Parakeet models
> because the ONNX graphs have dynamic input shapes, preventing CoreML from
> building optimised execution plans for ANE/GPU. CoreML claims nodes but
> runs them on CPU with overhead.

And the README's second line:

> Note: CoreML is unstable with this model. For Apple, use WebGPU EP … or
> CPU. But even CPU alone is significantly faster on my Mac M3 16GB compared
> to Whisper metal!

Source: <https://github.com/altunenes/parakeet-rs> (README and
`src/execution.rs`, as vendored at 0.3.7).

WebGPU, the crate's own alternative for Apple, is marked in the same file as
"experimental and may produce incorrect results". For a program that types
into the user's documents, silently wrong output is worse than slow output.

### 2.2 Is the int8 graph compatible?

ONNX Runtime's CoreML EP allows dynamic input shapes by default, with the
documented caveat that "performance may be negatively impacted"; a
`RequireStaticInputShapes` option exists to refuse them instead.
Source: <https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html>

Minion's graph is the worst case on both axes at once:

* **Dynamic shapes.** Utterances are 1–5 s of variable length. This is the
  exact condition the crate comment names.
* **int8.** `models.rs:52-83` downloads `encoder-model.int8.onnx` (652 MB)
  and `decoder_joint-model.int8.onnx`. CoreML's ANE path is float16; the
  nodes CoreML would most want to accelerate are precisely the ones
  quantisation rewrote into quantise/dequantise-wrapped integer matmuls,
  which are among the least well covered by the EP. The likely outcome is
  the one the crate describes: CoreML claims a partition and runs it on the
  CPU anyway, plus partitioning overhead.

### 2.3 Two costs specific to Minion

* **Memory.** `inference_config()` deliberately sets
  `session.disable_prepacking` and
  `session.use_device_allocator_for_initializers` — the pair that took the
  process from 1830 MB to 934 MB (`main.rs:452-462`, `CLAUDE.md`). Those are
  CPU-session settings; with a CoreML EP holding part of the graph they
  apply only to the CPU partition, and a compiled CoreML model is
  *additional* resident memory plus an on-disk cache directory. For a
  menu-bar app that already sits near 1 GB and unloads at idle to 405 MB,
  that is a straight regression risk against the number the project cares
  most about.
* **There is almost nothing to win.** The log format in `CLAUDE.md` shows
  ~142 ms per utterance. Even a perfect 2× is ~70 ms saved — invisible next
  to the segmenter's own silence timeout, and far below the point a person
  notices.

### 2.4 Recommendation: **no-go**

Three independent reasons, any one of them sufficient:

1. The upstream crate documents CoreML as slower and unstable *for this
   model*, and names Minion's exact graph shape as the cause.
2. The graph is int8 **and** dynamically shaped — the two things the CoreML
   EP handles worst, together.
3. The latency being optimised is already below the threshold where it
   matters.

Revisit only when both of these are true: parakeet-rs removes the warning,
**and** a static-shape (fixed-length buckets) fp16 export exists. At that
point the measurement to run is `minion corpus` wall-clock before and after
on real recordings — not a synthetic benchmark, and not a single utterance.
Changing the export also means new SHA-256 pins in `models.rs` **and** a new
`tests/corpus.rs` baseline; those two must move in the same commit or the
failure looks like a regression in the matcher.

WebGPU: also no-go, on "may produce incorrect results" alone.

---

## 3. Embedded LLM

### 3.1 Where Minion is now

`src/ai/` is complete and proven from the command line, off by default
(`backend = ""`), and already carries two **local** OpenAI-compatible
backends that need no key:

```rust
Preset { name: "ollama",   base_url: "http://localhost:11434/v1", model: "llama3.2",     needs_key: false },
Preset { name: "lmstudio", base_url: "http://localhost:1234/v1",  model: "local-model",  needs_key: false },
```
(`ai/openai_compat.rs:81-91`)

Requests go through `/usr/bin/curl` with the whole config on stdin
(`ai/mod.rs:879`); there is no HTTP crate in the tree. `[ai] use` already
separates the two purposes — `questions` and `unknown` (`ai/mod.rs:88-104`).

### 3.2 The three options

**A. Embed llama.cpp via `llama-cpp-2`.**
**B. Run a small LLM through the `ort` already in the tree.**
**C. Keep recommending Ollama / LM Studio.**

### 3.3 Memory

Minion is ~1 GB resident with the speech model loaded and 405 MB idle. A
small chat model on top of that:

* `llama3.2:1b` — 1.3 GB;
* `llama3.2:3b` — 2.0 GB.
  Source: <https://ollama.com/library/llama3.2>

In-process, that is added to Minion's own RSS — permanently, or behind a
second unload timer duplicating the one `ai::unload_if_idle` already has for
conversations and the one `main.rs` has for the speech model. A menu-bar app
at 3 GB is not something anyone leaves running all day, which is the entire
premise of this project.

Out of process, the memory is Ollama's: unloaded by Ollama's own idle timer,
and visible in Activity Monitor under a name the user recognises and can
kill. That last point is not a small thing for an app whose selling point is
that it is auditable.

### 3.4 Startup and contention

llama.cpp mmaps weights and starts fast, but the first token of a 1–3 B
model on CPU or Metal competes for the same cores as ASR. `inference_config`
deliberately runs `intra_threads: 2` to keep utterance latency low
(`main.rs:445`); an in-process LLM would have to be throttled the same way,
which is exactly the configuration a small model is slowest in.

### 3.5 Licensing

No obstacle on the code side, in any option:

* llama.cpp is MIT; `llama-cpp-2` is `MIT OR Apache-2.0`
  (<https://crates.io/crates/llama-cpp-2>, 0.1.156, 2026-09-02).
* Ollama is MIT (<https://github.com/ollama/ollama/blob/main/LICENSE>).

The constraint is the **weights**. Embedding means Minion has to ship or
download a model and take a position on its licence (Llama's community
licence, Qwen's, …), plus another 1–2 GB in `models.rs` with its own pinned
revision and SHA-256. Recommending Ollama means the *user* chooses the model
and accepts its terms — which is both less legal surface and more honest.

### 3.6 Build complexity

`llama-cpp-2` builds vendored C++ at build time and needs clang/libclang
(<https://crates.io/crates/llama-cpp-2>). That breaks a property this project
currently has and benefits from: **every dependency is Rust, plus
`/usr/bin/curl`.** `./build-app.sh` is `cargo build --release` and a
codesign; adding llama.cpp adds a large C++ build, a second native library
to sign, and one more artefact to keep out of git (see the standing rule
about never committing `.onnx` or `Minion.app`).

### 3.7 The `ort` route is not a shortcut

`ort` is already in the tree, but `ort` alone is not a text-generation
runtime: no KV-cache management, no sampler, no chat template. That is what
`onnxruntime-genai` provides, and it has no binding reachable from this
dependency tree. Writing the generation loop by hand is weeks, and it would
become the single largest module in the program — for a feature that is off
by default. **Option B is out.**

### 3.8 What a 1–3 B model can actually do for Minion

The two purposes are not equally suited, and `[ai] use` already lets them be
configured apart:

* **`Purpose::Unknown`** — pick a command name from a catalogue of ~100 and
  answer with strict JSON (`ai/mod.rs:491-534`). This is classification, not
  knowledge. A 1–3 B instruct model is adequate, and the surrounding code
  already defends itself: `parse_suggestion` tolerates fences and prose, and
  a name that is not in the catalogue is discarded no matter how confident
  the model sounded (`ai/mod.rs:509-513`). **Strong case.**
* **`Purpose::Questions`** — a one-or-two-sentence answer that `say` reads
  aloud. A 1–3 B model is the wrong tool: this is where confident wrong
  facts come from, the answer is *spoken*, so the user never sees a hedge,
  and the system prompt's "si no sabes algo, dilo en una frase"
  (`ai/mod.rs:83`) is the instruction small models follow worst.
  **Weak case.**

### 3.9 Recommendation: **C — keep recommending Ollama; do not embed**

The whole case for embedding is "one process instead of two". Against it:
+1.3–2.0 GB of resident memory in a program judged on exactly that number, a
C++ toolchain in a pure-Rust build, a model licence to take a position on,
and a second unload timer. Nothing in the list is a capability Ollama does
not already provide.

Concretely:

1. **Leave `src/ai/` as it is.** `ollama` and `lmstudio` already work and
   cost nothing while `backend` is empty.
2. **Make the local path discoverable rather than embedded.** A line in
   onboarding and in `minion ai status` saying that
   `backend = "ollama"`, `model = "llama3.2:3b"` keeps everything on the
   machine — the current status text explains how to turn the feature on
   (`ai/mod.rs:830`) but not that a fully local option exists.
3. **Recommend the two purposes separately,** which the config already
   supports: `use = ["unknown"]` against a local 1–3 B model is a good
   default (classification, validated against the catalogue, no facts
   asserted); `use = ["questions"]` deserves a larger model, local or not.
4. If in-process is ever revisited, it should be for `unknown` only, with
   the smallest model that works — and the honest framing is that it saves a
   process, not that it adds a capability.

---

## 4. Dead ends and silent assumptions found while reading

Ordered by how quietly they fail.

1. **`speech::default_voice` matches the literal `"es_ES"`**
   (`speech.rs:46`). A Mac carrying only `es_MX` or `es_AR` voices gets
   `None`, and `say` falls back to an English voice reading Spanish text.
   This is wrong **today**, independent of any multilingual work. Fix: match
   the `es` prefix and order `es_ES` first.
2. **`text::normalise` folds a fixed list and silently keeps everything
   else** (`text.rs:14-31`). Romanian `ș`/`ț`, Polish `ł`, Turkish `ı`,
   Greek and Cyrillic are `is_alphanumeric()`, so they survive unfolded and
   will never match a table entry written without them. Under Spanish this
   never fires. The first non-Spanish pack loaded fails one word at a time,
   with no log line to say so.
3. **`text::phonetic` is applied unconditionally to every app alias**
   (`commands.rs:991` and `commands.rs:1011`). It hard-codes Spanish
   orthography — `v`→`b`, `z`→`s`, `ll`→`y`, drop the final vowel. On
   non-Spanish text it produces confident nonsense and can merge two
   different names into one match. There is no way to turn it off.
4. **`shares_stem`'s `MIN_STEM = 6`** (`text.rs:316`) and `edit_distance`'s
   length bail-out of 2 (`text.rs:334`) are calibrated on Spanish word
   lengths. For German compounds or Finnish, a six-letter shared prefix
   covering three quarters of the shorter word is far too permissive, and it
   fails as a *wrong command run*, not as a miss.
5. **`text::keywords` canonicalises verbs before dropping fillers**, and the
   order is load-bearing — `"para"` is both a preposition and the imperative
   of *parar* (`text.rs:36-49`). Any port that copies the tables without the
   order reintroduces the bug where «para la música» became `[musica]`.
   The tables alone do not carry this.
6. **`"no"` is deliberately not in `FILLER`** (`spanish.rs:12`), because
   `session.rs:217` needs it to survive as an answer. Nothing in either file
   says so. A future tidy-up of the filler list would break question
   answering, and the failure would be a `Pending` question that can never
   be declined.
7. **The spoken-date grammar is an inline format string** —
   `format!("{day}, {} de {month}", now.day())` (`answers.rs:366`). Not
   data, and not obvious from the constants around it.
8. **Spanish typography is encoded in `dictation`'s atom kinds.** `¿` and
   `¡` are `opening()` atoms — space before, glued after
   (`dictation.rs:349-353`, `401`). A port that translated only the *words*
   would happily type `¿` into an English sentence.
9. **`commands::NUMBERED` matches the accent-folded `"pestana"`**
   (`commands.rs:407`). The table must hold the *folded* spelling. Writing
   the correct `"pestaña"` compiles, passes review, and silently never
   matches.
10. **`ParakeetTDT` has no language selection** (§1.1), so
    `transcribe_samples(…, None)` at `main.rs:979` is always auto-detect.
    "minimized the ventana" in the log is that, not a matcher bug. Recorded
    here so it is not re-diagnosed.
11. **`models.rs` pins the int8 encoder by SHA-256** (`models.rs:52-83`).
    Any experiment with a different export needs new hashes **and** a new
    `tests/corpus.rs` baseline in the same commit, or the test failure
    reads as a recognition regression.
12. **A `language` key in `config.toml` must be written before `[audio]`.**
    Top-level keys after a table header belong to that table, and
    `deny_unknown_fields` then invalidates the whole file — the trap already
    documented in `CLAUDE.md`, and one a new top-level key walks straight
    into.

---

## Sources

* Parakeet TDT 0.6b v3 model card — <https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3>
* ONNX export in use — <https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx>
* parakeet-rs (README, `src/execution.rs`, `Cargo.toml` features), 0.3.7 as vendored — <https://github.com/altunenes/parakeet-rs>
* ONNX Runtime CoreML execution provider — <https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html>
* `llama-cpp-2` (version, licence, build requirements) — <https://crates.io/crates/llama-cpp-2>
* Ollama licence — <https://github.com/ollama/ollama/blob/main/LICENSE>
* Llama 3.2 model sizes — <https://ollama.com/library/llama3.2>
