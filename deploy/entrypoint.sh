#!/bin/sh
set -eu

# A fresh named /data volume is root-owned. Prepare it before dropping
# privileges so SQLite, server data, and backups remain writable.
mkdir -p /data/servers /data/backups
chown -R app:app /data

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
