#!/usr/bin/env bash
# Creates a self-signed "Code Signing" certificate in the login keychain so local
# builds get a STABLE code-signing identity.
#
# Why: macOS ties Accessibility / Input Monitoring (TCC) grants to a binary's
# code-signing "designated requirement". An ad-hoc signature's requirement is
# the per-build cdhash, so every rebuild produces a new requirement and the OS
# forgets the grant — the app shows as enabled in System Settings yet
# AXIsProcessTrusted() is false. A stable self-signed identity keeps the
# requirement constant (identifier + cert leaf), so a grant survives rebuilds.
#
# Idempotent, no sudo. The resulting builds are NOT notarized (self-signed
# only), so they are for local use, not public distribution.
#
# Optional: KEYCHAIN_PASSWORD=<login-keychain-password> suppresses the one-time
# codesign key-access prompt via set-key-partition-list.
set -euo pipefail

CERT_NAME="${DEV_CERT_NAME:-Mousehop Dev}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -p codesigning "$KEYCHAIN" 2>/dev/null | grep -qF "$CERT_NAME"; then
    echo "Code-signing identity already present: $CERT_NAME"
    exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

cat > "$TMP/openssl.cnf" <<EOF
[req]
distinguished_name = req_distinguished_name
prompt = no
x509_extensions = v3_codesign

[req_distinguished_name]
CN = ${CERT_NAME}

[v3_codesign]
basicConstraints = CA:FALSE
keyUsage = digitalSignature
extendedKeyUsage = codeSigning
EOF

openssl req -new -x509 -days 3650 -nodes \
    -config "$TMP/openssl.cnf" \
    -keyout "$TMP/key.pem" \
    -out "$TMP/cert.pem"

# -legacy emits a PKCS#12 macOS `security import` accepts; OpenSSL 3 defaults are
# rejected by the Security framework. A non-empty password is used because
# `security import` rejects empty-password PKCS#12 ("MAC verification failed").
# Fall back to the default cipher when -legacy is unsupported (older openssl).
P12_PW="$(openssl rand -hex 16)"
openssl pkcs12 -export -legacy \
    -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
    -name "$CERT_NAME" -out "$TMP/cert.p12" -passout pass:"$P12_PW" 2>/dev/null \
    || openssl pkcs12 -export \
        -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
        -name "$CERT_NAME" -out "$TMP/cert.p12" -passout pass:"$P12_PW"

# Import cert + private key. -A lets codesign use the key; -T whitelists the tool.
security import "$TMP/cert.p12" -k "$KEYCHAIN" -P "$P12_PW" -A -T /usr/bin/codesign

# Best effort: suppress the one-time key-access prompt for codesign. Needs the
# login keychain password (pass it via KEYCHAIN_PASSWORD); when unset, the first
# codesign shows a prompt where you click "Always Allow".
if [ -n "${KEYCHAIN_PASSWORD:-}" ]; then
    security set-key-partition-list -S apple-tool:,apple:,codesign: \
        -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN" >/dev/null 2>&1 || true
fi

echo "Created self-signed code-signing identity: $CERT_NAME"
security find-identity -p codesigning "$KEYCHAIN" | grep -F "$CERT_NAME" || true
