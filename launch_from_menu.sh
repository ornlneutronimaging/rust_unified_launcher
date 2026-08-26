#!/usr/bin/env bash
# Menu-entry wrapper for the Neutron Imaging Launcher.
#
# The system .desktop entry (not editable without root) wraps its command in a
# gnome-terminal, which otherwise stays open for the whole app session. This
# script detaches the real launch script and exits immediately, so that
# terminal closes as soon as it opens. Output (build messages, errors) goes to
# a per-user log instead: ~/.cache/rust_unified_launcher/launch.log
#
# For an interactive launch with visible output, run
# launch_unified_launcher.sh directly.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"

LOG_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/rust_unified_launcher"
mkdir -p "$LOG_DIR"
LOG_FILE="$LOG_DIR/launch.log"

# Keep only the latest launch in the log.
{
    echo "=== Launch $(date '+%Y-%m-%d %H:%M:%S') ==="
} > "$LOG_FILE"

# The terminal's whole systemd cgroup is killed when its window closes, so a
# plain setsid is not enough — hand the launcher to the user session manager,
# which runs it outside that cgroup. Pass the display-related variables along:
# the user manager's environment does not reliably have them.
ENV_ARGS=()
for var in DISPLAY WAYLAND_DISPLAY XAUTHORITY DBUS_SESSION_BUS_ADDRESS; do
    [[ -n "${!var:-}" ]] && ENV_ARGS+=("--setenv=$var=${!var}")
done

# KillMode=process: when the portal exits, systemd would otherwise kill every
# process left in the unit's cgroup — i.e. all the apps the portal launched
# (their setsid() changes the session, not the cgroup). Only the main process
# (the portal itself, already gone) may be targeted; launched apps live on.
if systemd-run --user --collect --quiet "${ENV_ARGS[@]}" \
        --property=KillMode=process \
        --property=StandardOutput=append:"$LOG_FILE" \
        --property=StandardError=append:"$LOG_FILE" \
        "$REPO_DIR/launch_unified_launcher.sh" "$@" 2>> "$LOG_FILE"; then
    exit 0
fi

# Fallback (no user manager, e.g. some remote sessions): detach with setsid.
# This survives only if the parent is not inside a to-be-killed cgroup.
echo "systemd-run failed; falling back to setsid" >> "$LOG_FILE"
setsid "$REPO_DIR/launch_unified_launcher.sh" "$@" \
    < /dev/null >> "$LOG_FILE" 2>&1 &
