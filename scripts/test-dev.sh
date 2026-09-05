#!/usr/bin/env bash
# Exercise feedback failures without compiling Rust or touching the desktop.
set -euo pipefail
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf -- "$fixture"' EXIT
mkdir -p "$fixture/bin"
cat > "$fixture/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$DEV_TEST_ARGS"
case "$DEV_TEST_RESULT" in
  pass) echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;' ;;
  empty) echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;' ;;
  fail) echo 'compiler failure' >&2; exit 23 ;;
esac
EOF
chmod +x "$fixture/bin/cargo"
export PATH="$fixture/bin:$PATH"
export XDG_STATE_HOME="$fixture/state"
export DEV_TEST_ARGS="$fixture/args"

DEV_TEST_RESULT=pass "$PROJECT_DIR/scripts/dev.sh" test ui desktop 'filter with spaces' > "$fixture/output"
printf '%s\n' test --locked -p agentdictate-ui --test desktop --features test-support 'filter with spaces' > "$fixture/expected"
cmp "$fixture/expected" "$fixture/args"
grep -q 'exit_status: 0;' "$fixture/output"

for outcome in empty fail; do
  status=0
  DEV_TEST_RESULT="$outcome" "$PROJECT_DIR/scripts/dev.sh" test core core > "$fixture/output" 2>&1 || status=$?
  if [[ "$outcome" == empty ]]; then
    [[ "$status" == 1 ]]
    grep -q 'FAILED: no tests passed' "$fixture/output"
  else
    [[ "$status" == 23 ]]
    grep -q 'compiler failure' "$fixture/output"
  fi
done
[[ "$(find "$XDG_STATE_HOME/agentdictate/checks" -type f | wc -l)" == 3 ]]
echo 'Developer feedback checks passed: arguments, headless features, logs, zero tests, and failure status.'
