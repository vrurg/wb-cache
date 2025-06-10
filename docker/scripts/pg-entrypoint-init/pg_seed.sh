#!/usr/bin/env bash

###### Initialize the database ######
# This script is executed by the PostgreSQL Docker image during the initialization phase.

set -e

if [ -z "$WBCACHE_PG_DB_PREFIX" ]; then
    echo "WBCACHE_PG_DB_PREFIX is not set. Please set it to the desired database prefix."
    exit 1
fi

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" -d postgres <<-EOSQL
CREATE DATABASE "${WBCACHE_PG_DB_PREFIX}_plain";
CREATE DATABASE "${WBCACHE_PG_DB_PREFIX}_cached";
EOSQL