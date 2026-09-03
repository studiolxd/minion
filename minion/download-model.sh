#!/usr/bin/env bash
# Downloads the speech models ahead of time. Optional: Minion fetches them
# itself on first run. ~670 MB, int8 quantised.
#
# Parakeet TDT 0.6b v3 covers 25 languages including Spanish. The int8 build
# is used because inference runs on CPU: CoreML support in parakeet-rs is
# still marked unstable.
#
# Pinned to a specific commit of each Hugging Face repository, not
# `resolve/main`, and checked against a known SHA-256 before being kept —
# `main` is a mutable ref, and a swapped model would quietly change what
# gets transcribed and who "the owner" is for the speaker check. Keep this
# in step with src/models.rs, which pins and checks the same files when
# Minion downloads them itself.
set -euo pipefail

# Same place the application downloads to on first run, so doing this by
# hand and letting it do it are interchangeable.
DIR="$HOME/Library/Application Support/Minion/model"
SPEECH_REVISION="8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce"
BASE="https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/$SPEECH_REVISION"
SPEAKER_REVISION="a2f3dcb1c8702caccc7a55ceb57f5e8d1842112b"
SPEAKER_URL="https://huggingface.co/Wespeaker/wespeaker-ecapa-tdnn512-LM/resolve/$SPEAKER_REVISION/voxceleb_ECAPA512_LM.onnx"
SPEAKER_SHA256="d71b85d9b48058ef68004f04f1b78acebefb9dfcf542e19b976a12a5ad1f10b0"

mkdir -p "$DIR"
chmod 700 "$DIR/.." 2>/dev/null || true
cd "$DIR"

# Downloads to a .partial name, verifies it against the expected hash, and
# only then moves it into place — so a truncated or substituted download
# never gets accepted as the real thing, and a failed run leaves nothing
# for the next one to be confused by.
download() {
  local remote="$1" local_name="$2" sha256="$3"
  if [ -f "$local_name" ]; then
    echo "  ✓ $local_name (already here)"
    return
  fi
  echo "  ↓ $local_name"
  curl -fL --proto '=https' --tlsv1.2 --max-time 3600 --progress-bar \
    -o "$local_name.partial" "$BASE/$remote"
  echo "$sha256  $local_name.partial" | shasum -a 256 -c - >/dev/null
  mv "$local_name.partial" "$local_name"
}

echo "Downloading Parakeet TDT 0.6b v3 into $DIR"
download config.json config.json \
  666903c76b9798caf2c210afd4f6cd60b08a8dbf9800ec8d7a3bc0d2148ac466
download vocab.txt vocab.txt \
  d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d
download nemo128.onnx nemo128.onnx \
  a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f
download decoder_joint-model.int8.onnx decoder_joint-model.onnx \
  eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70
download encoder-model.int8.onnx encoder-model.onnx \
  6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09

# Speaker model: 24 MB, tells your voice from anyone else's. Optional —
# without it, Minion answers whoever speaks the wake word.
if [ ! -f speaker.onnx ]; then
  echo "  ↓ speaker.onnx"
  curl -fL --proto '=https' --tlsv1.2 --max-time 3600 --progress-bar \
    -o speaker.onnx.partial "$SPEAKER_URL"
  echo "$SPEAKER_SHA256  speaker.onnx.partial" | shasum -a 256 -c - >/dev/null
  mv speaker.onnx.partial speaker.onnx
else
  echo "  ✓ speaker.onnx (already here)"
fi

echo
echo "Done. Total: $(du -sh "$DIR" | cut -f1)"
