#!/bin/sh
# Stand-in for `docker` when driving `agentctl watch`'s [l] Logs view without a daemon.
#
# Put a copy named exactly `docker` first on PATH (see SKILL.md). It answers the two startup
# probes and then streams compose-shaped log lines forever.
#
# The grandchild fork is the point, not an accident: the real `docker compose` is a CLI
# PLUGIN, so `docker` forks `docker-compose` and hands it the stdout pipe. A teardown that
# only kills the direct child strands that grandchild holding the pipe — the ux.10-A orphan
# bug. Reproducing the fork here is what makes the orphan check meaningful.
#
# Env:
#   FAKE_DOCKER_PIDFILE  where to write the grandchild pid (for the orphan assertion)
#   FAKE_DOCKER_MODE     stream (default) | flood | giant | empty | fail | hang
#   FAKE_DOCKER_PROJECT  project prefix in the log prefix (default: agentos)

PROJECT="${FAKE_DOCKER_PROJECT:-agentos}"
MODE="${FAKE_DOCKER_MODE:-stream}"

case "$MODE" in
  fail) exit 1 ;;                       # non-zero probe -> no compose project detected
  hang) sleep 300 ;;                    # wedged daemon -> the probe deadline must fire
esac

case "$*" in
  *"compose ps --all --quiet"*)
    [ "$MODE" = empty ] && exit 0       # exit 0 with no ids -> empty project, view stays absent
    echo "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd01"
    echo "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd02"
    echo "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd03"
    ;;
  *"compose ps --services"*)
    printf 'cos\nagent\nqdrant\n'
    ;;
  *"compose logs"*)
    # Grandchild streams; parent only waits (the CLI-plugin shape).
    sh -c '
      P="'"$PROJECT"'"
      M="'"$MODE"'"
      if [ "$M" = flood ]; then
        # 5000 lines as fast as possible: exercises the ring cap + channel backpressure.
        ts=2026-01-01T00:00:00.000000000Z
        i=0; while [ $i -lt 5000 ]; do i=$((i+1)); echo "$P-cos-1  | $ts flood line $i"; done
      fi
      if [ "$M" = giant ]; then
        # One ~12 KB record with NO newline: exercises the byte cap + resync.
        ts=2026-01-01T00:00:00.000000000Z
        printf "%s-agent-1  | %s GIANT" "$P" "$ts"
        j=0; while [ $j -lt 1200 ]; do j=$((j+1)); printf "0123456789"; done
        printf "\n"
      fi
      n=0
      while :; do
        n=$((n+1))
        ts=$(date -u +%Y-%m-%dT%H:%M:%S.000000000Z)
        echo "$P-cos-1  | $ts chief of staff tick $n"
        echo "$P-agent-1  | $ts agent heartbeat $n"
        echo "$P-qdrant-1  | $ts qdrant collection ready $n"
        sleep 1
      done' &
    [ -n "$FAKE_DOCKER_PIDFILE" ] && echo $! > "$FAKE_DOCKER_PIDFILE"
    wait
    ;;
  *)
    echo "fake docker: unhandled args: $*" >&2
    exit 1
    ;;
esac
