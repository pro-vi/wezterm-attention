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
WEZTERM_ATTENTION_TTY_INPUT_GUARD="$root/tests/tty_input_guard.py" cargo test -- --test-threads=1
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
node tests/fixtures/lifecycle/check-coverage.mjs
python3 tests/fixtures/lifecycle/check-compatibility.py

# Test-only runtimes are isolated from the checkout and live installations.
gate_scratch=$(mktemp -d "${TMPDIR:-/tmp}/attention-lifecycle-gate.XXXXXX")
trap 'rm -rf "$gate_scratch"' EXIT HUP INT TERM
pi_baseline=${ATTENTION_PI_BASELINE_ROOT:-}
if [ -z "$pi_baseline" ]; then
  npm install --prefix "$gate_scratch/pi" --ignore-scripts --no-audit --no-fund \
    --package-lock=false --save=false @earendil-works/pi-coding-agent@0.80.5 \
    @earendil-works/pi-agent-core@0.80.5 @earendil-works/pi-ai@0.80.5 @earendil-works/pi-tui@0.80.5
  pi_baseline="$gate_scratch/pi/node_modules/@earendil-works/pi-coding-agent"
fi
node -e 'const p=process.argv[1]; if(require(p+"/package.json").version!=="0.80.5") process.exit(1)' "$pi_baseline"
ATTENTION_PI_RUNTIME_ROOT="$pi_baseline" cargo test --test lifecycle_spec actual_pi_runner_dispatches_through_the_real_writer -- --exact
mkdir -p "$gate_scratch/typecheck/node_modules/@earendil-works"
ln -s "$pi_baseline" "$gate_scratch/typecheck/node_modules/@earendil-works/pi-coding-agent"
cp pi/index.ts "$gate_scratch/typecheck/index.ts"
"$root/node_modules/.bin/tsc" --noEmit --target ES2022 --module ESNext --moduleResolution Bundler \
  --skipLibCheck --types bun --typeRoots "$root/node_modules/@types" "$gate_scratch/typecheck/index.ts"

xterm_module=${ATTENTION_XTERM_MODULE:-}
if [ -z "$xterm_module" ]; then
  npm install --prefix "$gate_scratch/xterm" --ignore-scripts --no-audit --no-fund \
    --package-lock=false --save=false @xterm/headless@5.5.0
  xterm_module="$gate_scratch/xterm/node_modules/@xterm/headless"
fi
ATTENTION_XTERM_MODULE="$xterm_module" cargo test --test lifecycle_spec \
  native_codex_queued_input_is_not_blocked_by_a_pending_question -- --ignored --exact

baseline_binary=${ATTENTION_BASELINE_RUST:-}
if [ -z "$baseline_binary" ]; then
  mkdir "$gate_scratch/baseline"
  baseline_commit=$(node -p 'require("./tests/fixtures/lifecycle/compatibility.json").baseline_commit')
  git archive --output="$gate_scratch/baseline.tar" "$baseline_commit" \
    Cargo.toml Cargo.lock src tests protocol bin shell plugin scripts
  tar -xf "$gate_scratch/baseline.tar" -C "$gate_scratch/baseline"
  cargo build --release --manifest-path "$gate_scratch/baseline/Cargo.toml" --target-dir "$root/target/frozen-lifecycle"
  baseline_binary="$root/target/frozen-lifecycle/release/attention"
fi
cargo build --release
python3 tests/rust/measure.py --baseline-rust "$baseline_binary" --rust-binary "$root/target/release/attention"

find README.md docs -type f -name '*.md' -print | while IFS= read -r markdown; do
  "$HOME/.local/bin/md" map "$markdown" >/dev/null
done

git diff --check
git diff --cached --check
