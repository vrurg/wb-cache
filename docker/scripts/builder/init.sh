#!/usr/bin/env bash

if [ -n "$WBCACHE_CLEAR_TARGET" ]; then
    cargo clean
fi
