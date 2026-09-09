#!/usr/bin/env sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

sh -n bin/attention examples/hook.sh tests/run_wezterm_smoke.sh tests/gate.sh \
  tests/fixtures/consumer-migration/claude-stop.sh \
  tests/fixtures/consumer-migration/codex-stop.sh
bash -n shell/wezterm-attention.bash
zsh -n shell/wezterm-attention.zsh
! rg -n 'python3' bin src shell pi plugin examples scripts
cargo fmt --check
cargo clippy --all-targets -- -D warnings
WEZTERM_ATTENTION_TTY_INPUT_GUARD="$root/tests/tty_input_guard.py" cargo test
python3 -m py_compile tests/fixtures/v2/check.py \
  tests/fixtures/consumer-migration/bridge_reader.py \
  tests/fixtures/consumer-migration/check.py tests/provider_contact_hook.py \
  tests/rust/check_python_test_map.py tests/rust/measure.py tests/rust/measure_spec.py
python3 tests/rust/check_python_test_map.py
python3 -m unittest tests/rust/measure_spec.py
python3 tests/fixtures/v2/check.py
python3 tests/fixtures/consumer-migration/check.py
luajit tests/auto_clear_spec.lua
env -u WEZTERM_ATTENTION_DIR -u WEZTERM_ATTENTION_ROOT -u WEZTERM_PANE \
  -u WEZTERM_UNIX_SOCKET bun test tests/pi_extension.test.ts
bun run typecheck
node tests/pi_node_runtime.mjs
sh tests/run_wezterm_smoke.sh

find README.md docs -type f -name '*.md' -print | while IFS= read -r markdown; do
  "$HOME/.local/bin/md" map "$markdown" >/dev/null
done

git diff --check
git diff --cached --check
