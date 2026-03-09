# Rove Core Plugins Developer Guide

Welcome to the internal engineering docs for **Rove Plugins**.

## Overview

Plugins are compiled to standalone WebAssembly (`.wasm`) payloads leveraging Extism. They run in strictly separated Sandbox contexts with 0 host-OS visibility by default.

To construct a new signed plugin, review the `.github/workflows` to ensure proper CD bundling into `registry/`.
