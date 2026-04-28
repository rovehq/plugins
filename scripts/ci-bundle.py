#!/usr/bin/env python3
"""
CI bundler for official Rove plugins.

Generates schema-v1 plugin bundles installable via `rove plugin install`:
  - plugin-package.json  (payload_hash + payload_signature)
  - release.json         (signed with official key)
  - index.json           (per-plugin, signed with official key)
  - registry.json        (catalog, signed with official key)

Usage:
  python3 scripts/ci-bundle.py \
    --plugin-dir browser-cdp \
    --wasm-file target/wasm32-wasip1/release/browser_cdp.wasm \
    --registry-dir artifacts/registry \
    --official-key-hex <hex>

  # After all plugins processed:
  python3 scripts/ci-bundle.py \
    --finalize-catalog \
    --registry-dir artifacts/registry \
    --official-key-hex <hex>
"""

import argparse
import glob
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path


PKCS8_ED25519_PREFIX = bytes.fromhex("302e020100300506032b657004220420")


def make_pem(key_input: str, pem_path: str):
    import base64
    cleaned = key_input.strip()
    # Accept: 64-char hex seed, 32-byte hex seed, or base64-encoded PKCS8 DER (48 bytes)
    try:
        raw = bytes.fromhex(cleaned)
        if len(raw) == 48 and raw[:16] == PKCS8_ED25519_PREFIX:
            # Stored as hex of the full PKCS8 DER — strip prefix
            raw = raw[16:]
        elif len(raw) != 32:
            raise ValueError(f"Ed25519 seed must be 32 bytes, got {len(raw)}")
        der = PKCS8_ED25519_PREFIX + raw
    except ValueError:
        # Try base64 (DER stored as base64 without PEM headers)
        try:
            der = base64.b64decode(cleaned)
            if len(der) == 48 and der[:16] == PKCS8_ED25519_PREFIX:
                pass  # valid PKCS8 DER
            else:
                print(f"ERROR: key is neither valid hex seed nor base64 PKCS8 DER (len={len(der)})", file=sys.stderr)
                sys.exit(1)
        except Exception as e2:
            print(f"ERROR: cannot parse key (first chars={repr(cleaned[:8])}): {e2}", file=sys.stderr)
            sys.exit(1)
    pem = "-----BEGIN PRIVATE KEY-----\n" + base64.encodebytes(der).decode() + "-----END PRIVATE KEY-----\n"
    Path(pem_path).write_text(pem)


def sign_json(json_file: str, pem_path: str):
    d = json.loads(Path(json_file).read_text())
    d.pop("signature", None)
    d.pop("signed_at", None)
    canon = json.dumps(d, sort_keys=True, separators=(",", ":")).encode()
    canon_file = "/tmp/rove_sign_canon.bin"
    sig_file = "/tmp/rove_sign_out.bin"
    Path(canon_file).write_bytes(canon)
    subprocess.run(
        ["openssl", "pkeyutl", "-sign", "-rawin", "-inkey", pem_path,
         "-in", canon_file, "-out", sig_file],
        check=True, capture_output=True
    )
    sig = Path(sig_file).read_bytes().hex()
    d["signed_at"] = int(time.time())
    d["signature"] = sig
    Path(json_file).write_text(json.dumps(d, indent=2))


def sha256_hex(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def sign_payload(wasm_path: str, pem_path: str) -> str:
    # Engine: team_public_key.verify(file_hash.as_bytes(), &signature)
    # Signs the SHA-256 hex string (as bytes), not the binary hash
    file_hash = sha256_hex(wasm_path)
    hash_file = "/tmp/rove_payload_hash.bin"
    sig_file = "/tmp/rove_payload_sig.bin"
    Path(hash_file).write_bytes(file_hash.encode())
    subprocess.run(
        ["openssl", "pkeyutl", "-sign", "-rawin", "-inkey", pem_path,
         "-in", hash_file, "-out", sig_file],
        check=True, capture_output=True
    )
    return Path(sig_file).read_bytes().hex()


def bundle_plugin(plugin_dir: str, wasm_file: str, registry_dir: str, pem_path: str, json_sign_pem: str = None):
    plugin_path = Path(plugin_dir)
    manifest_file = plugin_path / "manifest.json"
    if not manifest_file.exists():
        print(f"  SKIP: no manifest.json in {plugin_dir}")
        return

    manifest = json.loads(manifest_file.read_text())
    version = manifest["version"]
    plugin_name = plugin_path.name
    plugin_id = plugin_name.replace("_", "-")
    wasm_name = plugin_name.replace("-", "_")
    trust_tier = manifest.get("trust_tier", "Official")

    wasm_path = Path(wasm_file)
    if not wasm_path.exists():
        print(f"  SKIP: wasm not found at {wasm_file}")
        return

    payload_hash = sha256_hex(str(wasm_path))
    payload_sig = sign_payload(str(wasm_path), pem_path)

    bundle_dir = Path(registry_dir) / plugin_id / version
    bundle_dir.mkdir(parents=True, exist_ok=True)

    import shutil
    shutil.copy(wasm_path, bundle_dir / f"{wasm_name}.wasm")
    shutil.copy(manifest_file, bundle_dir / "manifest.json")

    runtime_rel = ""
    if (plugin_path / "runtime.json").exists():
        shutil.copy(plugin_path / "runtime.json", bundle_dir / "runtime.json")
        runtime_rel = "runtime.json"

    readme_rel = ""
    if (plugin_path / "README.md").exists():
        shutil.copy(plugin_path / "README.md", bundle_dir / "README.md")
        readme_rel = "README.md"

    pkg = {
        "id": plugin_id,
        "artifact": f"{wasm_name}.wasm",
        "runtime_config": runtime_rel,
        "payload_hash": payload_hash,
        "payload_signature": payload_sig,
        "enabled": True,
    }
    (bundle_dir / "plugin-package.json").write_text(json.dumps(pkg, indent=2))

    ts = int(time.time())
    rel = {
        "id": plugin_id,
        "name": plugin_name,
        "version": version,
        "plugin_type": manifest.get("plugin_type", "Workspace"),
        "trust_tier": trust_tier,
        "generated_at": ts,
        "signed_at": 0,
        "signature": "",
        "artifact": f"{wasm_name}.wasm",
        "runtime_config": runtime_rel,
    }
    rel_file = str(bundle_dir / "release.json")
    Path(rel_file).write_text(json.dumps(rel, indent=2))
    sign_json(rel_file, json_sign_pem or pem_path)

    bundle_rel = f"{plugin_id}/{version}"
    index_file = Path(registry_dir) / plugin_id / "index.json"
    try:
        idx = json.loads(index_file.read_text())
    except Exception:
        idx = {
            "schema_version": "1",
            "generated_at": ts,
            "signed_at": 0,
            "signature": "",
            "plugin": {
                "id": plugin_id,
                "name": plugin_name,
                "plugin_type": manifest.get("plugin_type", "Workspace"),
                "trust_tier": trust_tier,
                "latest_version": version,
                "index_path": f"{plugin_id}/index.json",
            },
            "versions": [],
        }

    idx["generated_at"] = ts
    idx["plugin"]["latest_version"] = version
    idx["versions"] = [v for v in idx["versions"] if v["version"] != version]
    idx["versions"].insert(0, {
        "version": version,
        "published_at": ts,
        "bundle_path": bundle_rel,
        "manifest_path": f"{bundle_rel}/manifest.json",
        "package_path": f"{bundle_rel}/plugin-package.json",
        "runtime_path": f"{bundle_rel}/runtime.json" if runtime_rel else None,
        "artifact_path": f"{bundle_rel}/{wasm_name}.wasm",
        "artifact_sidecar_path": None,
        "readme_path": f"{bundle_rel}/README.md" if readme_rel else None,
        "release_path": f"{bundle_rel}/release.json",
    })
    try:
        from packaging.version import Version as V
        idx["versions"].sort(key=lambda x: V(x["version"]), reverse=True)
    except Exception:
        idx["versions"].sort(key=lambda x: x["version"], reverse=True)

    index_file.write_text(json.dumps(idx, indent=2))
    sign_json(str(index_file), json_sign_pem or pem_path)

    print(f"  bundled: {plugin_id} v{version}")


def finalize_catalog(registry_dir: str, pem_path: str, json_sign_pem: str = None):
    ts = int(time.time())
    catalog_file = Path(registry_dir) / "registry.json"
    try:
        catalog = json.loads(catalog_file.read_text())
    except Exception:
        catalog = {"schema_version": "1", "generated_at": ts, "signed_at": 0, "signature": "", "plugins": []}

    catalog["generated_at"] = ts
    plugins = []
    for f in glob.glob(str(Path(registry_dir) / "*/index.json")):
        try:
            idx = json.loads(Path(f).read_text())
            plugins.append(idx["plugin"])
        except Exception:
            pass
    plugins.sort(key=lambda x: x.get("name", ""))
    catalog["plugins"] = plugins
    catalog_file.write_text(json.dumps(catalog, indent=2))
    sign_json(str(catalog_file), json_sign_pem or pem_path)
    print(f"  catalog: {len(plugins)} plugins")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--plugin-dir")
    parser.add_argument("--wasm-file")
    parser.add_argument("--registry-dir", required=True)
    parser.add_argument("--official-key-hex")
    parser.add_argument("--pem-file", default="/tmp/rove_official.pem")
    parser.add_argument("--json-sign-key-hex", help="Key for signing JSON files (community key for community plugins)")
    parser.add_argument("--json-sign-pem-file", default="/tmp/rove_json_sign.pem")
    parser.add_argument("--finalize-catalog", action="store_true")
    args = parser.parse_args()

    key_hex = args.official_key_hex or os.environ.get("OFFICIAL_KEY_HEX", "")
    if not key_hex:
        print("ERROR: --official-key-hex or OFFICIAL_KEY_HEX required", file=sys.stderr)
        sys.exit(1)

    Path(args.registry_dir).mkdir(parents=True, exist_ok=True)

    if not Path(args.pem_file).exists() or os.path.getsize(args.pem_file) == 0:
        make_pem(key_hex, args.pem_file)

    json_sign_pem = None
    if args.json_sign_key_hex:
        json_sign_hex = args.json_sign_key_hex.strip()
        if not Path(args.json_sign_pem_file).exists() or os.path.getsize(args.json_sign_pem_file) == 0:
            make_pem(json_sign_hex, args.json_sign_pem_file)
        json_sign_pem = args.json_sign_pem_file

    if args.finalize_catalog:
        finalize_catalog(args.registry_dir, args.pem_file, json_sign_pem)
    elif args.plugin_dir and args.wasm_file:
        bundle_plugin(args.plugin_dir, args.wasm_file, args.registry_dir, args.pem_file, json_sign_pem)
    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
