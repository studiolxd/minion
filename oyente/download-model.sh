#!/usr/bin/env bash
# Downloads the speech model. ~670 MB, int8 quantised.
#
# Parakeet TDT 0.6b v3 covers 25 languages including Spanish. The int8 build
# is used because inference runs on CPU: CoreML support in parakeet-rs is
# still marked unstable.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/model"
BASE="https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main"

mkdir -p "$DIR"
cd "$DIR"

download() {
  local remote="$1" local_name="$2"
  if [ -f "$local_name" ]; then
    echo "  ✓ $local_name (already here)"
    return
  fi
  echo "  ↓ $local_name"
  curl -fL --progress-bar -o "$local_name" "$BASE/$remote"
}

echo "Downloading Parakeet TDT 0.6b v3 into $DIR"
download config.json config.json
download vocab.txt vocab.txt
download nemo128.onnx nemo128.onnx
download decoder_joint-model.int8.onnx decoder_joint-model.onnx
download encoder-model.int8.onnx encoder-model.onnx

echo
echo "Done. Total: $(du -sh "$DIR" | cut -f1)"
