#!/usr/bin/env bash
# backup_from_spark.sh — pull the booth's event data off the Spark onto THIS
# machine, so there's a copy that doesn't share the Spark's disk.
#
# Backs up the gitignored, Spark-local trial data (nothing here is in git):
#   - captures/                       (guilty-trial keepsake frames, host bind mount)
#   - counsel recordings/             (lawyer-phone call audio, if any)
#   - transcripts.jsonl + booth.log   (sealed inside the booth-logs Docker volume)
#
# It is incremental and idempotent — safe to run every few minutes for the whole
# event. Captures rsync near-instantly; the crown-jewel transcripts.jsonl is
# pulled from the container and ALSO snapshotted (dated) whenever it changes, so
# a truncation/clobber never costs you the history.
#
#   scripts/backup_from_spark.sh            one pull
#   WETCOURT_BACKUP_DIR=/path scripts/backup_from_spark.sh   custom destination
#   SPARK_HOST=<addr> scripts/backup_from_spark.sh           force a host
#
# Destination defaults to ~/wetcourt-event-backups (OUTSIDE the repo, so the
# gitignored data can never be accidentally committed).

set -euo pipefail

SPARK_USER=${SPARK_USER:-kaddison}
SPARK_DIR=${SPARK_DIR:-WetCourt}                # relative to $HOME on the Spark
CONTAINER=${CONTAINER:-orchestrator}
LAN_ADDR=192.168.123.125
TS_ADDR=100.86.115.53
DEST=${WETCOURT_BACKUP_DIR:-$HOME/wetcourt-event-backups}

probe() { ssh -o ConnectTimeout=3 -o BatchMode=yes "$SPARK_USER@$1" true 2>/dev/null; }

if [[ -n "${SPARK_HOST:-}" ]]; then
  HOST=$SPARK_HOST
elif probe "$LAN_ADDR"; then
  HOST=$LAN_ADDR
elif probe "$TS_ADDR"; then
  HOST=$TS_ADDR; echo "LAN unreachable, using Tailscale ($TS_ADDR)"
else
  echo "Spark unreachable on LAN ($LAN_ADDR) and Tailscale ($TS_ADDR)." >&2
  echo "Override with SPARK_HOST=<addr>, or bring up Tailscale, and retry." >&2
  exit 1
fi

REMOTE="$SPARK_USER@$HOST"
stamp=$(date +%Y%m%d-%H%M%S)
mkdir -p "$DEST/captures" "$DEST/recordings" "$DEST/logs/snapshots"
echo "backup: $REMOTE  ->  $DEST   ($stamp)"

# --- 1. host bind-mounted dirs: straight incremental rsync (no --delete: a
#        backup is additive — never drop a copy just because the Spark did) ---
rsync_dir() {  # <remote-subpath> <local-subdir>
  local src="$REMOTE:$SPARK_DIR/$1/" dst="$DEST/$2/"
  if ssh "$REMOTE" "test -d '$SPARK_DIR/$1'"; then
    rsync -az --info=stats1 "$src" "$dst" && echo "  rsync ok: $2"
  else
    echo "  skip (absent on Spark): $1"
  fi
}
rsync_dir "orchestrator/captures"                    "captures"
rsync_dir "orchestrator/crates/counsel/recordings"   "recordings"

# --- 2. Docker-volume files: pull via the container, write atomically, and
#        refuse to overwrite a good backup with an empty/failed read ---
pull_volume_file() {  # <container-path> <local-basename> [snapshot]
  local cpath="$1" name="$2" snap="${3:-}" tmp="$DEST/logs/.$2.tmp"
  if ! ssh "$REMOTE" "docker exec $CONTAINER cat '$cpath'" > "$tmp" 2>/dev/null; then
    echo "  skip ($CONTAINER not running or file absent): $name"; rm -f "$tmp"; return
  fi
  if [[ ! -s "$tmp" ]]; then
    echo "  skip (empty read, keeping prior copy): $name"; rm -f "$tmp"; return
  fi
  mv "$tmp" "$DEST/logs/$name"
  echo "  pulled: logs/$name ($(wc -l < "$DEST/logs/$name" | tr -d ' ') lines)"
  # dated snapshot only when content actually changed since the last one
  if [[ "$snap" == "snapshot" ]]; then
    local latest; latest=$(ls -1t "$DEST/logs/snapshots/${name%.*}-"*.${name##*.} 2>/dev/null | head -1 || true)
    if [[ -z "$latest" ]] || ! cmp -s "$DEST/logs/$name" "$latest"; then
      cp "$DEST/logs/$name" "$DEST/logs/snapshots/${name%.*}-$stamp.${name##*.}"
      echo "  snapshot: ${name%.*}-$stamp.${name##*.}"
    fi
  fi
}
pull_volume_file "/var/log/booth/transcripts.jsonl" "transcripts.jsonl" snapshot
pull_volume_file "/var/log/booth/booth.log"         "booth.log"

echo "done."
