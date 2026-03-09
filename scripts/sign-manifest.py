#!/usr/bin/env python3
"""
Sign manifest for Rove
Signs manifest.json with Ed25519 team private key.

Usage:
  # Local development (placeholder signature):
  python3 sign-manifest.py

  # Production signing (real signature):
  ROVE_SIGNING_KEY=<hex_private_key> python3 sign-manifest.py --env prod

  # Sign with key file:
  python3 sign-manifest.py --key-file path/to/private_key.hex --env prod

The signing process:
  1. Load manifest JSON
  2. Remove "signature" and "signed_at" fields
  3. Serialize as canonical JSON: sorted keys, compact separators
  4. Sign canonical bytes with Ed25519 private key
  5. Write signature back to manifest

Canonical JSON format (must match Rust's serde_json::to_string on Value):
  - Keys sorted alphabetically
  - No whitespace: separators=(',', ':')
  - Python: json.dumps(data, sort_keys=True, separators=(',', ':'))
  - Rust: serde_json::to_string(&value) where Value uses BTreeMap
"""

import json
import os
import sys
import datetime
from pathlib import Path

def canonicalize(data: dict) -> bytes:
    """Produce canonical JSON bytes for signing.

    Removes signature-related fields, then serializes with:
    - Sorted keys (alphabetical)
    - Compact separators (no whitespace)

    This matches Rust's serde_json::to_string() on serde_json::Value
    (which uses BTreeMap for sorted keys).
    """
    # Remove signature fields
    clean = {k: v for k, v in data.items() if k not in ("signature", "signed_at")}
    # Canonical: sorted keys, compact
    return json.dumps(clean, sort_keys=True, separators=(',', ':')).encode('utf-8')


def sign_bytes(data: bytes, private_key_hex: str) -> str:
    """Sign data with Ed25519 private key, return hex signature."""
    try:
        from nacl.signing import SigningKey
    except ImportError:
        print("Error: PyNaCl is required for production signing.", file=sys.stderr)
        print("Install it with: pip install pynacl", file=sys.stderr)
        sys.exit(1)

    private_key_bytes = bytes.fromhex(private_key_hex)
    if len(private_key_bytes) != 32:
        print(f"Error: Private key must be 32 bytes, got {len(private_key_bytes)}", file=sys.stderr)
        sys.exit(1)

    signing_key = SigningKey(private_key_bytes)
    signed = signing_key.sign(data)
    return signed.signature.hex()


def load_private_key(key_file: str = None) -> str:
    """Load private key from env var or file. Returns hex string."""
    # Try env var first
    key = os.environ.get("ROVE_SIGNING_KEY") or os.environ.get("ROVE_PLUGIN_PRIVATE_KEY")
    if key:
        return key.strip()

    # Try key file
    if key_file:
        path = Path(key_file)
        if not path.exists():
            print(f"Error: Key file not found: {key_file}", file=sys.stderr)
            sys.exit(1)

        content = path.read_text().strip()
        # If .bin file, convert to hex
        if key_file.endswith('.bin'):
            content = path.read_bytes().hex()
        return content

    return None


def sign_manifest_prod(manifest_path: Path, private_key_hex: str):
    """Sign manifest with real Ed25519 signature for production."""
    print(f"Signing manifest (production mode): {manifest_path}")

    with open(manifest_path, 'r') as f:
        manifest = json.load(f)

    # Canonicalize
    canonical = canonicalize(manifest)
    print(f"  Canonical bytes: {len(canonical)} bytes")

    # Sign
    signature = sign_bytes(canonical, private_key_hex)
    print(f"  Signature: {signature[:32]}...")

    # Write back
    manifest["signature"] = signature
    manifest["signed_at"] = datetime.datetime.utcnow().isoformat() + "Z"

    with open(manifest_path, 'w') as f:
        json.dump(manifest, f, indent=2)

    print(f"  Manifest signed successfully")

    # Verify round-trip: re-read and re-canonicalize should produce same bytes
    with open(manifest_path, 'r') as f:
        verify = json.load(f)
    verify_canonical = canonicalize(verify)
    assert verify_canonical == canonical, "Round-trip canonicalization mismatch!"
    print(f"  Round-trip verification: OK")


def sign_manifest_local(manifest_path: Path):
    """Sign manifest with placeholder signature for local development."""
    print(f"Signing manifest (local development mode): {manifest_path}")

    with open(manifest_path, 'r') as f:
        manifest = json.load(f)

    manifest["signature"] = "LOCAL_DEV_PLACEHOLDER_SIGNATURE"
    manifest["signed_at"] = "local-development"

    with open(manifest_path, 'w') as f:
        json.dump(manifest, f, indent=2)

    print(f"  Manifest signed with dev placeholder")
    print("  Note: Production builds require a real Ed25519 signature")


def main():
    import argparse

    parser = argparse.ArgumentParser(description="Sign Rove manifest with Ed25519")
    parser.add_argument("--env", choices=["dev", "prod"], default="dev",
                       help="Environment: dev (placeholder) or prod (real signature)")
    parser.add_argument("--key-file", help="Path to private key file (.hex or .bin)")
    parser.add_argument("--manifest", help="Path to manifest.json (default: auto-detect)")
    args = parser.parse_args()

    # Find manifest
    if args.manifest:
        manifest_path = Path(args.manifest)
    else:
        base_dir = Path(__file__).parent.parent.resolve()
        manifest_path = base_dir / "manifest" / "manifest.json"

    if not manifest_path.exists():
        print(f"Error: Manifest not found: {manifest_path}", file=sys.stderr)
        print("Run build-manifest.py first", file=sys.stderr)
        return 1

    try:
        if args.env == "prod":
            private_key = load_private_key(args.key_file)
            if not private_key:
                print("Error: No private key found for production signing.", file=sys.stderr)
                print("Provide via ROVE_SIGNING_KEY env var or --key-file", file=sys.stderr)
                return 1
            sign_manifest_prod(manifest_path, private_key)
        else:
            sign_manifest_local(manifest_path)
        return 0
    except Exception as e:
        print(f"Error signing manifest: {e}", file=sys.stderr)
        import traceback
        traceback.print_exc()
        return 1


if __name__ == "__main__":
    sys.exit(main())
