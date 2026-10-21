#!/usr/bin/env bash
set -euo pipefail

KEY_PATH="${1:-update-signing-key.pem}"
if [[ -e "$KEY_PATH" ]]; then
    echo "Refusing to overwrite existing key: $KEY_PATH" >&2
    exit 1
fi

umask 077
openssl genpkey -algorithm ED25519 -out "$KEY_PATH"
PUBLIC_DER="$(mktemp)"
trap 'rm -f "$PUBLIC_DER"' EXIT
openssl pkey -in "$KEY_PATH" -pubout -outform DER -out "$PUBLIC_DER" >/dev/null 2>&1

echo "Private key: $KEY_PATH"
echo "Add its contents to the GitHub Actions secret FLUXDB_UPDATE_SIGNING_KEY."
echo "Public key (set as GitHub Actions variable FLUXDB_UPDATE_PUBLIC_KEY):"
tail -c 32 "$PUBLIC_DER" | base64
