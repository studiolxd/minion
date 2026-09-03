# Recognition corpus

A fixed set of recordings, and what Minion should make of each one, so
recognition can be measured instead of guessed at from the log.

## What lives here

- `*.wav` — 16 kHz mono, the same shape [`audio::save_recording`](../src/audio.rs)
  writes. **Never committed** — see below.
- `corpus.toml` — for every WAV, what it should transcribe to and what
  decision it should produce. Committed.
- `baseline.toml` — the result of the last `minion corpus <dir> --save`.
  Committed, so a regression shows up in `git diff` as well as in
  `cargo test`.

## Building a corpus

1. Set `save_recordings = true` in `config.toml` for a session (see
   `CLAUDE.md` — turn it off again afterwards; it is off by default on
   purpose).
2. Say a representative run of commands — the usual ones, a few that
   should fail, a few in someone else's voice if you want the speaker
   check covered too.
3. Bootstrap a first `corpus.toml`:

   ```sh
   minion corpus --from-log \
     ~/Library/"Application Support"/Minion/recordings \
     minion/corpus
   ```

   This pairs each WAV with the log line written for it, copies the
   matched WAVs into `minion/corpus/`, and writes `minion/corpus/corpus.toml`.
   It is a starting point, not a finished corpus — read what it wrote.
   In particular:
   - A `heard` line with no quoted phrase (the default, since
     `log_ignored_speech` is off) leaves `expected_transcript` as
     `"unknown"`. Fill in what was actually said if that file is meant to
     test the transcript, not just that it was ignored.
   - Files the log has no matching line for (log rotated, wrong machine)
     are silently skipped.
4. Edit `corpus.toml` by hand: fix any entry the bootstrap guessed wrong,
   remove recordings you would rather not keep, add `owner_voice = false`
   to any that are someone else's voice.
5. Run it and freeze a baseline:

   ```sh
   minion corpus minion/corpus --save
   ```

## `corpus.toml` format

```toml
[[entry]]
file = "2026-09-03_10-15-00.wav"
expected_transcript = "minion abre chrome"
expected_decision = "abrir Chrome"
owner_voice = true

[[entry]]
file = "2026-09-03_10-15-08.wav"
expected_transcript = "minium so fuddy"
expected_decision = "Unrecognised"
```

- `expected_transcript` — what Parakeet should produce, wake word
  included. `"unknown"` when the point of the file is that nothing
  should be transcribed.
- `expected_decision` — what [`corpus::describe`](../src/corpus.rs) would
  print for the decision: `"abrir Chrome"`, `"cerrar Safari"`, `"buscar
  «algo» en Spotify"`, and so on, mirroring `Done.description` from
  `commands::perform` without actually running the action. Two special
  values: `"Unrecognised"` for a phrase that starts with the wake word but
  matches nothing, `"Blank"` for audio Parakeet should fail to transcribe
  at all (not the same as `"Ignored"`, which is real speech correctly
  judged as not addressed to Minion).
- `owner_voice` — defaults to `true`. Set to `false` for a recording of
  someone else's voice, so the speaker check gets tested from both sides.

## Running it

```sh
minion corpus minion/corpus            # run, compare against baseline.toml
minion corpus minion/corpus --save     # run, then overwrite baseline.toml
```

Prints one row per file — word error rate, the speaker score (`!` marks
one under the configured threshold for an `owner_voice = true` entry),
expected vs. got decision, and the transcript actually produced — then
totals: `decision_accuracy`, `mean_wer`, `min_owner_score`.

Without `--save`, an existing `baseline.toml` is compared against: a file
whose decision used to be right and no longer is, or whose word error rate
got noticeably worse, is printed as a `REGRESSION` and the command exits
with an error. `tests/corpus.rs` runs exactly this, so a regression here
fails `cargo test` too — skipped, not failed, when this directory has no
`corpus.toml` or the recognition model is not present, since neither is in
git.

## Why the WAVs are not committed

They are the corpus owner's voice, recorded near their own machine. Only
`corpus.toml` (what was said, in text) and `baseline.toml` (numbers) are
useful to anyone else working on Minion, and only those two are safe to
publish. The repository's `.gitignore` excludes `minion/corpus/*.wav`
accordingly — keep the WAVs somewhere private if you want to keep them at
all.
