#!/bin/sh
set -eu

# The data root is a host-visible bind mount because child containers are
# created through the host Docker socket. Prepare only the manager's own
# subdirectories before dropping privileges; never recursively chown an
# unrelated host directory when the root is mounted at /data.
data_root="${PUMPKIN_MANAGER_DATA_ROOT:-/data}"
mkdir -p "$data_root/servers" "$data_root/backups"
chown app:app "$data_root" "$data_root/servers" "$data_root/backups"
if [ -f "$data_root/pumpkin-manager.db" ]; then
    chown app:app "$data_root/pumpkin-manager.db"
fi
# Repair ownership of existing per-server roots from older deployments. This
# is scoped to direct children of the manager's configured data directory.
for server_dir in "$data_root"/servers/*; do
    if [ -d "$server_dir" ] && [ ! -L "$server_dir" ]; then
        chown -R -h app:app "$server_dir"
    fi
done

# Docker Engine access is intentionally a privileged trust boundary. Match the
# mounted socket's group so the backend can use the narrow Bollard adapter while
# still running as the unprivileged app user.
if [ -S /var/run/docker.sock ]; then
    docker_gid="$(stat -c '%g' /var/run/docker.sock)"
    docker_group="$(awk -F: -v gid="$docker_gid" '$3 == gid { print $1; exit }' /etc/group)"
    if [ -z "$docker_group" ]; then
        docker_group="docker-host"
        addgroup -g "$docker_gid" "$docker_group"
    fi
    addgroup app "$docker_group" 2>/dev/null || true
fi

exec su-exec app /usr/local/bin/pumpkin-manager
