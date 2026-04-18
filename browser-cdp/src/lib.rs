//! Chrome CDP browser backend for Rove.
//!
//! This native tool provides browser automation via the Chrome DevTools
//! Protocol (CDP). It connects to Chrome/Chromium over WebSocket and
//! exposes four standard browser tools:
//!
//!   - `browse_url`      — navigate to a URL and return the page title
//!   - `read_page_text`  — get visible text of the current page
//!   - `click_element`   — click a DOM element by CSS selector
//!   - `fill_form_field` — fill a form field by CSS selector
//!
//! Three connection modes:
//!   - `ManagedLocal`   — spawn Chrome headlessly and own the process
//!   - `AttachExisting` — connect to an already-running Chrome via its CDP port
//!   - `RemoteCdp`      — connect to a remote CDP WebSocket endpoint
//!
//! This crate is designed to be loaded as a native tool (`.dylib`) by
//! Rove's `NativeRuntime`, but is also compiled into the engine binary
//! during the transition period (Phase 1).

pub mod cdp;

pub use cdp::CdpBackend;
