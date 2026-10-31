#!/usr/bin/env bash
# The runtime's smoke test, and the canary for libservo changes: runs a
# built helloworld on an Xvfb display of its own, with data folders of its
# own, until the app closes its window by itself; then checks that every
# line of expected-results.txt appeared among its RESULT lines and that it
# exited with status 0.
#
#   examples/helloworld/smoke-test.sh <helloworld binary> [log file]
#
# Build the binary first (`cargo build --release -p helloworld`, default
# features). Environment:
#   XVFB                    the Xvfb binary (default: Xvfb on the PATH)
#   HELLOWORLD_CLOSE_AFTER  seconds before the app closes its window (20)
#   SMOKE_TIMEOUT           seconds before a run that has not exited counts
#                           as hung and is killed (120)
set -uo pipefail

app=${1:?usage: smoke-test.sh <helloworld binary> [log file]}
expected=$(dirname "$0")/expected-results.txt
work=$(mktemp -d)
log=${2:-$work/app.log}
timeout=${SMOKE_TIMEOUT:-120}
xvfb_pid=
app_pid=
cleanup() {
  [ -n "$app_pid" ] && kill "$app_pid" 2>/dev/null
  [ -n "$xvfb_pid" ] && kill "$xvfb_pid" 2>/dev/null
  wait 2>/dev/null
  rm -rf "$work"
}
trap cleanup EXIT

# Xvfb picks a free display number itself and writes it to fd 3, so runs
# side by side do not collide.
"${XVFB:-Xvfb}" -displayfd 3 -screen 0 1024x768x24 -nolisten tcp \
  3> "$work/display" 2> "$work/xvfb.log" &
xvfb_pid=$!
for _ in $(seq 100); do [ -s "$work/display" ] && break; sleep 0.1; done
if [ ! -s "$work/display" ]; then
  echo "smoke test: Xvfb did not start"
  cat "$work/xvfb.log"
  exit 1
fi
display=:$(head -1 "$work/display")

mkdir -p "$work/home" "$work/data" "$work/config" "$work/cache"
DISPLAY=$display GDK_BACKEND=x11 HOME=$work/home \
  XDG_DATA_HOME=$work/data XDG_CONFIG_HOME=$work/config XDG_CACHE_HOME=$work/cache \
  HELLOWORLD_CLOSE_AFTER=${HELLOWORLD_CLOSE_AFTER:-20} \
  RUST_LOG=warn,console=debug,helloworld=info \
  "$app" > "$log" 2>&1 &
app_pid=$!

hung=
for _ in $(seq $((timeout * 10))); do
  kill -0 "$app_pid" 2>/dev/null || break
  sleep 0.1
done
if kill -0 "$app_pid" 2>/dev/null; then
  hung=1
  kill "$app_pid" 2>/dev/null
fi
wait "$app_pid"
code=$?
app_pid=

grep -o 'RESULT .*' "$log" > "$work/results"
echo "RESULT lines from $app on $display:"
cat "$work/results"
echo

missing=0
checked=0
while IFS= read -r pattern; do
  case $pattern in '' | '#'*) continue ;; esac
  checked=$((checked + 1))
  if ! grep -Eq -- "$pattern" "$work/results"; then
    echo "missing: $pattern"
    missing=$((missing + 1))
  fi
done < "$expected"

failed=
if [ -n "$hung" ]; then
  echo "the app did not exit within $timeout s; killed"
  failed=1
elif [ "$code" -ne 0 ]; then
  echo "the app exited with status $code"
  failed=1
fi
[ "$missing" -gt 0 ] && failed=1
if [ -n "$failed" ]; then
  echo "smoke test: FAILED ($missing of $checked expected lines missing, exit status $code); the app's log ends:"
  tail -40 "$log"
  exit 1
fi
echo "smoke test: passed ($checked of $checked expected lines, $(wc -l < "$work/results") RESULT lines, exit status 0)"
