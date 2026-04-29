//! Chrome CDP browser backend for Rove.
//!
//! Exposes the four standard browser tools — `browse_url`, `read_page_text`,
//! `click_element`, `fill_form_field` — via the Chrome DevTools Protocol.
//!
//! This crate compiles as a `cdylib` and is loaded by Rove's `NativeRuntime`
//! as a Workspace driver.  The engine discovers it via the `browser_backend`
//! declaration in `runtime.json` and routes browser tool calls through the
//! `BrowserBackend` trait bridge (`NativeBrowserDriverBackend` in the engine).
//!
//! # Runtime model
//!
//! `CoreTool::handle()` is a synchronous method but CDP calls are async.
//! The driver spawns a dedicated OS thread running its own single-thread
//! Tokio runtime.  Commands are sent over a `tokio::sync::mpsc` channel;
//! replies come back over `tokio::sync::oneshot` channels.  The calling
//! thread uses `tokio::task::block_in_place` (available on the engine's
//! multi-thread runtime) to send the command and wait for the reply without
//! blocking the engine's executor.

pub mod cdp;

pub use cdp::{CdpConfig, CdpMode};

use std::thread;

use sdk::core_tool::{CoreContext, CoreTool};
use sdk::errors::EngineError;
use sdk::tool_io::{ToolInput, ToolOutput};
use tokio::sync::{mpsc, oneshot};

use cdp::CdpBackend;

// ---------------------------------------------------------------------------
// Command protocol between the CoreTool handle and the driver thread
// ---------------------------------------------------------------------------

type ReplySender = oneshot::Sender<Result<String, String>>;

enum BrowserCmd {
    Navigate {
        url: String,
        reply: ReplySender,
    },
    PageText {
        reply: ReplySender,
    },
    Click {
        selector: String,
        reply: ReplySender,
    },
    Fill {
        selector: String,
        value: String,
        reply: ReplySender,
    },
    InspectForm {
        reply: ReplySender,
    },
    FillFormSmart {
        data: serde_json::Value,
        submit: bool,
        reply: ReplySender,
    },
    GetPageStructure {
        reply: ReplySender,
    },
    ExtractSemanticData {
        keys: Vec<String>,
        reply: ReplySender,
    },
    WaitForNetworkIdle {
        timeout_ms: u64,
        reply: ReplySender,
    },
    GetInteractiveElements {
        reply: ReplySender,
    },
    ClickById {
        id: u32,
        reply: ReplySender,
    },
    CheckBlockers {
        reply: ReplySender,
    },
    GetElementDiff {
        selector: String,
        reply: ReplySender,
    },
    ExtractTableToJson {
        selector: Option<String>,
        reply: ReplySender,
    },
    SaveSessionState {
        reply: ReplySender,
    },
    RestoreSessionState {
        state: String,
        reply: ReplySender,
    },
    InterceptApiResponse {
        url_pattern: String,
        reply: ReplySender,
    },
    GetInterceptedResponses {
        reply: ReplySender,
    },
    Stop,
}

// ---------------------------------------------------------------------------
// BrowserCdpTool — implements CoreTool, loaded by NativeRuntime
// ---------------------------------------------------------------------------

pub struct BrowserCdpTool {
    cmd_tx: mpsc::Sender<BrowserCmd>,
}

impl BrowserCdpTool {
    fn new() -> Self {
        Self::new_with_config(CdpConfig {
            mode: CdpMode::ManagedLocal,
            cdp_url: None,
            browser: None,
            user_data_dir: None,
            startup_url: None,
        })
    }

    /// Construct with ManagedLocal defaults — convenience for tests.
    pub fn new_for_test() -> Self {
        Self::new()
    }

    /// Construct with an explicit config — used by integration tests.
    pub fn new_with_config(config: CdpConfig) -> Self {
        let (tx, mut rx) = mpsc::channel::<BrowserCmd>(8);

        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("browser-cdp driver tokio runtime");

            let mut backend = CdpBackend::new(config);

            rt.block_on(async move {
                while let Some(cmd) = rx.recv().await {
                    match cmd {
                        BrowserCmd::Navigate { url, reply } => {
                            let result =
                                backend.navigate_impl(&url).await.map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::PageText { reply } => {
                            let result = backend.page_text_impl().await.map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::Click { selector, reply } => {
                            let result = backend
                                .click_impl(&selector)
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::Fill {
                            selector,
                            value,
                            reply,
                        } => {
                            let result = backend
                                .fill_field_impl(&selector, &value)
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::InspectForm { reply } => {
                            let result =
                                backend.inspect_form_impl().await.map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::FillFormSmart {
                            data,
                            submit,
                            reply,
                        } => {
                            let result = backend
                                .fill_form_smart_impl(data, submit)
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::GetPageStructure { reply } => {
                            let result = backend
                                .get_page_structure_impl()
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::ExtractSemanticData { keys, reply } => {
                            let result = backend
                                .extract_semantic_data_impl(keys)
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::WaitForNetworkIdle { timeout_ms, reply } => {
                            let result = backend
                                .wait_for_network_idle_impl(timeout_ms)
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::GetInteractiveElements { reply } => {
                            let result = backend
                                .get_interactive_elements_impl()
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::ClickById { id, reply } => {
                            let result = backend
                                .click_by_id_impl(id)
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::CheckBlockers { reply } => {
                            let result = backend
                                .check_blockers_impl()
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::GetElementDiff { selector, reply } => {
                            let result = backend
                                .get_element_diff_impl(selector)
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::ExtractTableToJson { selector, reply } => {
                            let result = backend
                                .extract_table_to_json_impl(selector)
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::SaveSessionState { reply } => {
                            let result = backend
                                .save_session_state_impl()
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::RestoreSessionState { state, reply } => {
                            let result = backend
                                .restore_session_state_impl(state)
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::InterceptApiResponse { url_pattern, reply } => {
                            let result = backend
                                .intercept_api_response_impl(url_pattern)
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::GetInterceptedResponses { reply } => {
                            let result = backend
                                .get_intercepted_responses_impl()
                                .await
                                .map_err(|e| e.to_string());
                            let _ = reply.send(result);
                        }
                        BrowserCmd::Stop => break,
                    }
                }
            });
        });

        Self { cmd_tx: tx }
    }

    fn dispatch(
        &self,
        cmd: BrowserCmd,
        rx: oneshot::Receiver<Result<String, String>>,
    ) -> Result<ToolOutput, EngineError> {
        let tx = self.cmd_tx.clone();

        // We are called from within the engine's multi-thread Tokio runtime.
        // block_in_place signals that this thread will block and lets Tokio
        // move other tasks to idle worker threads.
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                tx.send(cmd).await.map_err(|_| {
                    EngineError::ToolError("browser driver disconnected".to_string())
                })?;

                rx.await
                    .map_err(|_| {
                        EngineError::ToolError("browser driver did not respond".to_string())
                    })?
                    .map(|text| ToolOutput::json(serde_json::Value::String(text)))
                    .map_err(EngineError::ToolError)
            })
        })
    }
}

impl CoreTool for BrowserCdpTool {
    fn name(&self) -> &str {
        "browser-cdp"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn start(&mut self, _ctx: CoreContext) -> Result<(), EngineError> {
        Ok(())
    }

    fn stop(&mut self) -> Result<(), EngineError> {
        let tx = self.cmd_tx.clone();
        // best-effort stop signal — ignore error if the driver thread is already gone
        let _ = tx.try_send(BrowserCmd::Stop);
        Ok(())
    }

    fn handle(&self, input: ToolInput) -> Result<ToolOutput, EngineError> {
        let (reply_tx, reply_rx) = oneshot::channel::<Result<String, String>>();

        let cmd = match input.method.as_str() {
            "browse_url" => {
                let url = input
                    .param_str("url")
                    .map_err(|e| EngineError::ToolError(e.to_string()))?;
                BrowserCmd::Navigate {
                    url,
                    reply: reply_tx,
                }
            }
            "read_page_text" => BrowserCmd::PageText { reply: reply_tx },
            "click_element" => {
                let selector = input
                    .param_str("selector")
                    .map_err(|e| EngineError::ToolError(e.to_string()))?;
                BrowserCmd::Click {
                    selector,
                    reply: reply_tx,
                }
            }
            "fill_form_field" => {
                let selector = input
                    .param_str("selector")
                    .map_err(|e| EngineError::ToolError(e.to_string()))?;
                let value = input
                    .param_str("value")
                    .map_err(|e| EngineError::ToolError(e.to_string()))?;
                BrowserCmd::Fill {
                    selector,
                    value,
                    reply: reply_tx,
                }
            }
            "inspect_form" => BrowserCmd::InspectForm { reply: reply_tx },
            "fill_form_smart" => {
                let data =
                    input.params.get("data").cloned().ok_or_else(|| {
                        EngineError::ToolError("Missing parameter: data".to_string())
                    })?;
                let submit = input
                    .params
                    .get("submit")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                BrowserCmd::FillFormSmart {
                    data,
                    submit,
                    reply: reply_tx,
                }
            }
            "get_page_structure" => BrowserCmd::GetPageStructure { reply: reply_tx },
            "extract_semantic_data" => {
                let keys_value = input
                    .params
                    .get("keys")
                    .cloned()
                    .ok_or_else(|| EngineError::ToolError("Missing parameter: keys".to_string()))?;
                
                let keys = keys_value
                    .as_array()
                    .ok_or_else(|| EngineError::ToolError("keys must be an array".to_string()))?
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                
                BrowserCmd::ExtractSemanticData {
                    keys,
                    reply: reply_tx,
                }
            }
            "wait_for_network_idle" => {
                let timeout_ms = input
                    .params
                    .get("timeout_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10000);
                
                BrowserCmd::WaitForNetworkIdle {
                    timeout_ms,
                    reply: reply_tx,
                }
            }
            "get_interactive_elements" => BrowserCmd::GetInteractiveElements { reply: reply_tx },
            "click_by_id" => {
                let id = input
                    .params
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| EngineError::ToolError("Missing parameter: id".to_string()))? as u32;
                
                BrowserCmd::ClickById {
                    id,
                    reply: reply_tx,
                }
            }
            "check_blockers" => BrowserCmd::CheckBlockers { reply: reply_tx },
            "get_element_diff" => {
                let selector = input
                    .params
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::ToolError("Missing parameter: selector".to_string()))?
                    .to_string();
                
                BrowserCmd::GetElementDiff {
                    selector,
                    reply: reply_tx,
                }
            }
            "extract_table_to_json" => {
                let selector = input
                    .params
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                
                BrowserCmd::ExtractTableToJson {
                    selector,
                    reply: reply_tx,
                }
            }
            "save_session_state" => BrowserCmd::SaveSessionState { reply: reply_tx },
            "restore_session_state" => {
                let state = input
                    .params
                    .get("state")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::ToolError("Missing parameter: state".to_string()))?
                    .to_string();
                
                BrowserCmd::RestoreSessionState {
                    state,
                    reply: reply_tx,
                }
            }
            "intercept_api_response" => {
                let url_pattern = input
                    .params
                    .get("url_pattern")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| EngineError::ToolError("Missing parameter: url_pattern".to_string()))?
                    .to_string();
                
                BrowserCmd::InterceptApiResponse {
                    url_pattern,
                    reply: reply_tx,
                }
            }
            "get_intercepted_responses" => BrowserCmd::GetInterceptedResponses { reply: reply_tx },
            other => {
                return Err(EngineError::ToolError(format!(
                    "browser-cdp: unknown method '{other}'"
                )))
            }
        };

        self.dispatch(cmd, reply_rx)
    }
}

// ---------------------------------------------------------------------------
// FFI entry point — loaded by NativeRuntime via libloading
// ---------------------------------------------------------------------------

/// # Safety
///
/// Called by Rove's `NativeRuntime` via `libloading`. The returned pointer is
/// immediately wrapped in `Box::from_raw` by the runtime and must not be null.
#[no_mangle]
pub fn create_tool() -> *mut dyn CoreTool {
    let tool = Box::new(BrowserCdpTool::new());
    Box::into_raw(tool)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_method_returns_error() {
        let tool = BrowserCdpTool::new();
        let input = ToolInput::new("nonexistent_method");
        let err = tool.handle(input).unwrap_err();
        assert!(err.to_string().contains("unknown method"));
    }

    #[test]
    fn browse_url_requires_url_param() {
        let tool = BrowserCdpTool::new();
        let input = ToolInput::new("browse_url"); // no "url" param
        let err = tool.handle(input).unwrap_err();
        assert!(err.to_string().contains("url") || err.to_string().contains("Missing"));
    }

    #[test]
    fn create_tool_ffi_returns_non_null() {
        let ptr = create_tool();
        assert!(!ptr.is_null());
        // Re-box to avoid a leak in the test
        let _ = unsafe { Box::from_raw(ptr) };
    }
}
