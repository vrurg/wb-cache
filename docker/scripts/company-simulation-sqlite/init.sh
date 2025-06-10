#!/usr/bin/env bash

set -e

if [ "$WBCACHE_SAVE_SCRIPT" = "true" ]; then
    exec /usr/local/bin/company --save "$WBCACHE_SCRIPT_FILE"
else
    exec /usr/local/bin/company --sqlite
fi