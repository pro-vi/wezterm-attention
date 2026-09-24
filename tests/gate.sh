#!/usr/bin/env sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

# External programs are found on PATH. Override any of them with
# ATTENTION_TEST_NODE, ATTENTION_TEST_WEZTERM, ATTENTION_TEST_PYTHON3 or
# ATTENTION_TEST_CODEX when the one you want is not the first on PATH.
# ATTENTION_TEST_BASH names one more bash to drive the bash integration with,
# and ATTENTION_TEST_BASH_PREEXEC a local bash-preexec.sh to use instead of
# downloading the pinned one.
#
# Two integration tests drive a real Codex checkout, which this repository
# cannot supply. Set ATTENTION_CODEX_SOURCE to one to run them; without it they
# print SKIPPED and the rest of the gate is unaffected.

sh -n bin/attention examples/hook.sh tests/shell/run_wezterm_smoke.sh tests/gate.sh \
  tests/fixtures/consumer-migration/claude-stop.sh \
  tests/fixtures/consumer-migration/codex-stop.sh
bash -n shell/wezterm-attention.bash
zsh -n shell/wezterm-attention.zsh
! rg -n 'python3' bin src shell pi plugin examples scripts
cargo fmt --check
cargo clippy --all-targets -- -D warnings
# Cargo captures a passing test's output, so a test that skips itself does so
# silently. Say which way this run went before it happens.
if [ -n "${ATTENTION_CODEX_SOURCE:-}" ]; then
  printf 'gate: Codex contact tests ENABLED (ATTENTION_CODEX_SOURCE=%s)\n' "$ATTENTION_CODEX_SOURCE"
else
  printf 'gate: Codex contact tests SKIPPED (set ATTENTION_CODEX_SOURCE to a Codex checkout)\n'
fi
WEZTERM_ATTENTION_TTY_INPUT_GUARD="$root/tests/python/tty_input_guard.py" cargo test -- --test-threads=1
python3 -m py_compile tests/fixtures/v2/check.py \
  tests/fixtures/consumer-migration/bridge_reader.py \
  tests/fixtures/consumer-migration/check.py tests/python/provider_contact_hook.py \
  tests/python/measure.py tests/python/measure_spec.py tests/python/interactive_shell.py
python3 -m unittest tests/python/measure_spec.py
python3 tests/fixtures/v2/check.py
python3 tests/fixtures/consumer-migration/check.py
luajit tests/lua/auto_clear_spec.lua
# And again with the built writer hidden. libexec/attention-rs is a build
# artifact this repository does not track, so a machine that has built it can
# pass a suite that a fresh clone fails on its first run. Observed 2026-09-18:
# 31 of 130 Lua tests passed here and failed without it, across three green
# gate runs that had no way to notice. Remembering to check by hand is not a
# check.
gate_writer="$root/libexec/attention-rs"
gate_writer_aside="$root/libexec/.attention-rs.gate-aside"
if [ -e "$gate_writer" ]; then
  # Restores on any exit, including a failing suite, before the scratch trap
  # below replaces this one.
  trap 'if [ -e "$gate_writer_aside" ]; then mv -- "$gate_writer_aside" "$gate_writer"; fi' \
    EXIT HUP INT TERM
  mv -- "$gate_writer" "$gate_writer_aside"
  printf 'gate: repeating the Lua suite with the built writer hidden\n'
  luajit tests/lua/auto_clear_spec.lua
  mv -- "$gate_writer_aside" "$gate_writer"
  trap - EXIT HUP INT TERM
else
  printf 'gate: no built writer present, so the Lua suite above already ran without one\n'
fi
env -u WEZTERM_ATTENTION_DIR -u WEZTERM_ATTENTION_ROOT -u WEZTERM_PANE \
  -u WEZTERM_UNIX_SOCKET bun test tests/typescript/pi_extension.test.ts
bun run typecheck
node tests/javascript/pi_node_runtime.mjs
sh tests/shell/run_wezterm_smoke.sh
node tests/fixtures/lifecycle/check-coverage.mjs

# Test-only runtimes are isolated from the checkout and live installations.
gate_scratch=$(mktemp -d "${TMPDIR:-/tmp}/attention-lifecycle-gate.XXXXXX")
trap 'rm -rf "$gate_scratch"' EXIT HUP INT TERM

# The bash integration has to work beside bash-preexec, which owns the DEBUG
# trap wherever it is loaded; WezTerm's own shell integration carries a copy.
# Pinned by commit and checked by hash, because the file is sourced.
bash_preexec=${ATTENTION_TEST_BASH_PREEXEC:-}
if [ -z "$bash_preexec" ]; then
  bash_preexec="$gate_scratch/bash-preexec.sh"
  curl -fsSL -o "$bash_preexec" \
    https://raw.githubusercontent.com/rcaloras/bash-preexec/d866eeefdb8dfce075cbd4e37dc73c4deb4b0bd2/bash-preexec.sh
  if command -v sha256sum >/dev/null 2>&1; then
    bash_preexec_digest=$(sha256sum "$bash_preexec")
  else
    bash_preexec_digest=$(shasum -a 256 "$bash_preexec")
  fi
  if [ "${bash_preexec_digest%% *}" != 33de4e70ee84981d46e7d8a0e3105f1dd9affc9c4178594446cd96e7ef3b2752 ]; then
    printf 'gate: the downloaded bash-preexec.sh does not match its pinned hash\n' >&2
    exit 1
  fi
fi
# The bash on PATH, and /bin/bash when that is a different one: bash 3.2 on
# macOS. ATTENTION_TEST_BASH adds one more.
set -- "$(command -v bash)"
if [ -x /bin/bash ] && [ "$1" != /bin/bash ]; then set -- "$@" /bin/bash; fi
if [ -n "${ATTENTION_TEST_BASH:-}" ]; then set -- "$@" "$ATTENTION_TEST_BASH"; fi
for bash_shell in "$@"; do
  sh tests/shell/bash_integration_spec.sh "$bash_shell" "$bash_preexec"
done

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
  # Fixed measurement reference, not a supported reader/writer version. It is the
  # first commit that writes lifecycle snapshots, so it produces the same record
  # set as the candidate; an earlier one writes a third of the bytes and the
  # ratio below would score that difference instead of a regression.
  baseline_commit=01c4a43445ac4f583a5a8932cef22965aaa8f0cf
  git archive --output="$gate_scratch/baseline.tar" "$baseline_commit" \
    Cargo.toml Cargo.lock src tests protocol bin shell plugin scripts
  tar -xf "$gate_scratch/baseline.tar" -C "$gate_scratch/baseline"
  cargo build --release --manifest-path "$gate_scratch/baseline/Cargo.toml" --target-dir "$root/target/performance-baseline"
  baseline_binary="$root/target/performance-baseline/release/attention"
fi
cargo build --release
python3 tests/python/measure.py --baseline-rust "$baseline_binary" --rust-binary "$root/target/release/attention"


git diff --check
git diff --cached --check
