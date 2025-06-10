#!/usr/bin/env bash

if [ "$WBCACHE_SAVE_SCRIPT" = "true" ]; then
    /usr/local/bin/company --save "$WBCACHE_SCRIPT_FILE"
else
    /usr/local/bin/company --pg
fi

rc=$?

# Try to shutdown the PostgreSQL server gracefully.
sshpass -p 'cigIbUjmotgemByp' ssh postgres "pg_down"

exit $rc