# Rove Official Plugins

This repository contains the officially maintained, first-party WebAssembly (`.wasm`) plugins designed for the Rove ecosystem. These plugins are compiled using the Extism PDK and act as highly-constrained host function adapters, enforcing strict system boundaries.

## 🛡️ Architecture & Security

Every compiled plugin strictly implements the Rove SDK bindings. Plugins execute in an isolated WASM sandbox with zero implicit OS-level access:

1. **Network**: Only explicit URL domains injected by the host are reachable.
2. **Filesystem**: Plugins cannot open arbitrary file handles. They must call Host functions, which undergo canonicalization and "workspace-only" validations by the RiskAssessor.
3. **Subprocess**: All shell commands invoked by terminal plugins are parsed and cleansed of injection vulnerabilities before execution.

## 🛠️ Building Plugins

Ensure you have the WebAssembly target installed:

```bash
rustup target add wasm32-unknown-unknown
```

To compile all official tools into their optimized payloads:

```bash
cargo build --release --target wasm32-unknown-unknown
```

_(GitHub Actions handles pushing the compiled payloads directly to the central registry for public consumption)._

## 📦 Ecosystem Context

| System             | Technology                                                                                      | Description                                     | Link                         |
| ------------------ | ----------------------------------------------------------------------------------------------- | ----------------------------------------------- | ---------------------------- |
| **Engine Core**    | <img src="https://cdn.simpleicons.org/rust/white" width="18" align="center"/> Rust              | The host orchestrator that loads these plugins. | [`/core/`](../core/)         |
| **Registry Hub**   | <img src="https://cdn.simpleicons.org/cloudflare/F38020" width="18" align="center"/> Cloudflare | Dynamic OTA binary synchronization.             | [`/registry/`](../registry/) |
| **Developer Docs** | <img src="https://cdn.simpleicons.org/markdown/white" width="18" align="center"/> Docs          | Comprehensive module documentation.             | [`/docs/dev/`](./docs/dev/)  |
