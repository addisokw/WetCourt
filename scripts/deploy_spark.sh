#!/usr/bin/env bash
# deploy_spark.sh — sync the local checkout onto the Spark's live copy.
#
#   scripts/deploy_spark.sh              sync only
#   scripts/deploy_spark.sh -n           dry run (show what would change)
#   scripts/deploy_spark.sh --restart    sync + restart orchestrator/counsel
#                                        (enough for persona/config/crimes edits —
#                                        those are bind mounts, no rebuild needed)
#   scripts/deploy_spark.sh --build      sync + rebuild source-built images
#                                        (orchestrator, vision, counsel) + up -d
#
# Host: tries the LAN address first, falls back to Tailscale. Override with
#   SPARK_HOST=<addr> scripts/deploy_spark.sh
#
# What is deliberately NOT synced (and never deleted on the Spark):
#   - Spark-local runtime state, via explicit two-sided excludes: .env
#     (LITELLM_MASTER_KEY lives ONLY there), captures/, print_templates.json,
#     transcripts.jsonl, booth.log, counsel recordings/, vision/models/
#     (pre-downloaded pose model for offline use)
#   - orchestrator/calibration/  — tracked in git, but on the Spark it holds
#     console-tuned PER-DEVICE hardware calibration (squirt fire_ms, turret
#     boresight, neck trims). Deploying local copies would revert the booth.
#   - everything else gitignored is hidden from transfer (target/,
#     node_modules/, eval reports, ...). NB: the gitignore filter is
#     sender-side only — anything Spark-local that must survive --delete needs
#     an explicit --exclude below, not just a gitignore entry.
#
# NOTE: personas/ and crimes/ DO sync — git is their source of truth. If someone
# saved persona edits or curated crimes from the booth console since the last
# pull, this overwrites them (the dry run will show it).
#
# Caveat: the gitignore filter is applied with rsync semantics, which don't
# honor gitignore's `!` re-includes — tracked .wav fixtures (sample_plea.wav,
# counsel assets) are skipped by the sync. They land on the Spark via git, so
# this only matters if you add a NEW tracked .wav; deploy that one by hand.

set -euo pipefail

SPARK_USER=${SPARK_USER:-kaddison}
SPARK_DIR=${SPARK_DIR:-WetCourt}           # relative to $HOME on the Spark
LAN_ADDR=192.168.123.125
TS_ADDR=100.86.115.53
REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)

DRY= ; RESTART= ; BUILD=
for arg in "$@"; do
  case "$arg" in
    -n|--dry-run) DRY=-n ;;
    --restart)    RESTART=1 ;;
    --build)      BUILD=1 ;;
    -h|--help)    sed -n '2,28p' "$0"; exit 0 ;;
    *) echo "unknown arg: $arg (see --help)" >&2; exit 2 ;;
  esac
done

probe() { ssh -o ConnectTimeout=3 -o BatchMode=yes "$SPARK_USER@$1" true 2>/dev/null; }

if [[ -n "${SPARK_HOST:-}" ]]; then
  HOST=$SPARK_HOST
elif probe "$LAN_ADDR"; then
  HOST=$LAN_ADDR
elif probe "$TS_ADDR"; then
  HOST=$TS_ADDR; echo "LAN unreachable, using Tailscale ($TS_ADDR)"
else
  echo "Spark unreachable on LAN ($LAN_ADDR) and Tailscale ($TS_ADDR)." >&2
  echo "Override with SPARK_HOST=<addr> if it lives somewhere else today." >&2
  echo "(probe assumes key-based ssh; with password auth, set SPARK_HOST to skip it)" >&2
  exit 1
fi

echo "syncing $REPO_ROOT -> $SPARK_USER@$HOST:~/$SPARK_DIR ${DRY:+(dry run)}"
# NOTE: --exclude is two-sided (hides from transfer AND protects from
# --delete); the .gitignore filter is sender-side only and protects nothing.
rsync -az $DRY --delete --info=stats1,del,name1 \
  --exclude '.git/' \
  --exclude '.env' \
  --exclude '*.env.local' \
  --exclude 'orchestrator/calibration/' \
  --exclude 'captures/' \
  --exclude 'print_templates.json' \
  --exclude 'transcripts.jsonl' \
  --exclude 'booth.log' \
  --exclude 'recordings/' \
  --exclude 'vision/models/' \
  --filter=':- .gitignore' \
  "$REPO_ROOT"/ "$SPARK_USER@$HOST:$SPARK_DIR/"

[[ -n "$DRY" ]] && exit 0

compose() { ssh "$SPARK_USER@$HOST" "cd ~/$SPARK_DIR/dgx-ai-stack && docker compose $*"; }

if [[ -n "$BUILD" ]]; then
  echo "rebuilding source-built services on the Spark..."
  compose build orchestrator vision counsel
  compose up -d
elif [[ -n "$RESTART" ]]; then
  echo "restarting orchestrator + counsel (bind-mounted config/personas reload on boot)..."
  compose restart orchestrator counsel
else
  echo "synced. containers NOT restarted — run with --restart to reload personas/config,"
  echo "or --build if Rust/Python source changed."
fi
