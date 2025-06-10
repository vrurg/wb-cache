#!/usr/bin/env bash

set -e

echo "Starting SSHD."
/usr/sbin/sshd

echo "Starting PostgreSQL ($POSTGRES_USER:$POSTGRES_PASSWORD)."
chmod +x /docker-entrypoint-initdb.d/*.sh
echo "Setting the latency to ${WBCACHE_PG_NET_DELAY}"
tc qdisc add dev eth0 root netem delay ${WBCACHE_PG_NET_DELAY}
exec docker-entrypoint.sh postgres