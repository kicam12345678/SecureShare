#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
lan_addresses=$(ip -4 -o addr show scope global | awk '{print $4}' | cut -d/ -f1 || true)
if [[ -n "${lan_addresses}" ]]; then
    while IFS= read -r address; do
        printf 'SecureShare on this network: http://%s:8000\n' "${address}"
    done <<< "${lan_addresses}"
fi
exec cargo run --release
