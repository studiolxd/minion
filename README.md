# Voice control in Spanish, on macOS

**[minion/](minion/)** — the working project. Speak Spanish to your Mac,
hands free, entirely offline. Lives in the menu bar.

```bash
cd minion
./download-model.sh
./install.sh
```

Then say «minion, abre Chrome».

## How it got here

Three attempts, two of them retired. Their code is in the history rather
than the tree, but what they cost in surprises is worth keeping:

**Talon Voice** — outstanding command recognition, but its Conformer engine
is English-only and no Spanish model exists. Along the way: actions in
Talon's core are *declared but empty* (the community repo implements them),
so `edit.select_all` appears to exist and fails at runtime; a Spanish ISO
keyboard makes `cmd-[` fail silently because key codes are positional; and
an app can run with zero windows, where `focus()` swaps the menu bar
without showing anything.

**Handy + a Python bridge** — understood Spanish perfectly, but push-to-talk.
Its `external_script` paste method is documented as "Linux only" and hidden
from the macOS interface, yet works fine when written straight into the
settings file.

**Minion**, first named Oyente — Parakeet TDT v3 for Spanish recognition,
always listening, in Rust. What the other two each had half of.
