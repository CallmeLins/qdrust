#!/bin/sh
#
# Put /data right, then run the server as an unprivileged user.
#
# The image runs as uid 10001, and SQLite in WAL mode has to write the database
# together with its -wal/-shm sidecars. Copying /data out of one container and
# into another — which is what a root-run SQLite service lets people do, and what
# someone arriving from one will have done already — leaves those files owned by
# whoever did the copying. The server then starts, answers /ready, serves reads
# and fails every write, because SQLite only discovers it cannot write when
# something writes. A mount made the wrong way should not present itself as a
# working server.
#
# So the container starts as root, repairs the ownership when it is wrong, and
# drops to uid 10001 before exec. The server process itself never has root, and
# nothing in this script runs after it starts.
#
# Two ways out, for anyone who would rather not have a root phase at all:
#
#   docker run --user 10001:10001 ...   # or `user:` in compose. `id -u` is then
#                                       # not 0 and everything here is skipped.
#   -e QDRUST_CHOWN=0                   # start as root, but skip only the chown.
#
# Neither is needed in the ordinary case; see docs/deployment.md.
set -eu

if [ "$(id -u)" != "0" ]; then
    # Started with an explicit --user: the caller owns the ownership question.
    exec "$@"
fi

if [ "${QDRUST_CHOWN:-1}" != "0" ] && [ -e /data ]; then
    # Recursive only when the directory itself is wrong, which is exactly what a
    # copy leaves behind. A tree that is already correct pays one stat per start
    # rather than a walk, and an operator who mounted something of their own
    # under /data does not have it re-owned on every restart.
    if [ "$(stat -c '%u' /data)" != "$(id -u qdrust)" ]; then
        echo "qdrust: /data belongs to uid $(stat -c '%u' /data), taking it for uid $(id -u qdrust)" >&2
        # Not fatal: on a read-only mount there is nothing to repair, and the
        # server reports the unwritable database itself, with the uids in it.
        chown -R qdrust:qdrust /data || true
    fi
fi

# setpriv rather than su: no shell, no PAM, no environment rewriting, and the
# server stays PID 1 so signals reach it directly.
exec setpriv --reuid=qdrust --regid=qdrust --clear-groups -- "$@"
