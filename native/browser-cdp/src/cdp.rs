//! CDP WebSocket session and browser backend implementation.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use sdk::browser::BrowserBackend;
use sdk::errors::EngineError;
use tokio::process::Child;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// Connection mode for the CDP backend.
#[derive(Debug, Clone)]
pub enum CdpMode {
    /// Rove spawns Chrome headlessly and owns the process.
    ManagedLocal,
    /// Connect to an already-running Chrome via its CDP port.
    AttachExisting,
    /// Connect to a remote CDP WebSocket endpoint.
    RemoteCdp,
}

/// Configuration for a CDP backend instance.
#[derive(Debug, Clone)]
pub struct CdpConfig {
    pub mode: CdpMode,
    /// CDP URL (required for RemoteCdp, optional for AttachExisting).
    pub cdp_url: Option<String>,
    /// Override path to Chrome binary.
    pub browser: Option<String>,
    /// Chrome user data directory.
    pub user_data_dir: Option<String>,
    /// URL to open on launch.
    pub startup_url: Option<String>,
}

// ---------------------------------------------------------------------------
// CDP WebSocket session
// ---------------------------------------------------------------------------

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct CdpSession {
    ws: WsStream,
    next_id: u64,
}

impl CdpSession {
    async fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;

        let req = serde_json::json!({"id": id, "method": method, "params": params});
        self.ws
            .send(Message::Text(req.to_string().into()))
            .await
            .context("Failed to send CDP command")?;

        loop {
            match tokio::time::timeout(Duration::from_secs(15), self.ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let parsed: serde_json::Value =
                        serde_json::from_str(&text).context("CDP response is not valid JSON")?;
                    if parsed.get("id").and_then(|v| v.as_u64()) == Some(id) {
                        if let Some(error) = parsed.get("error") {
                            return Err(anyhow!("CDP error for {}: {}", method, error));
                        }
                        return Ok(parsed
                            .get("result")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null));
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(e))) => return Err(anyhow!("CDP WebSocket error: {}", e)),
                Ok(None) => return Err(anyhow!("CDP WebSocket closed unexpectedly")),
                Err(_) => {
                    return Err(anyhow!(
                        "CDP command '{}' timed out after 15 seconds",
                        method
                    ))
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CdpBackend
// ---------------------------------------------------------------------------

/// Chrome DevTools Protocol browser backend.
pub struct CdpBackend {
    config: CdpConfig,
    session: Option<CdpSession>,
    chrome_process: Option<Child>,
}

impl CdpBackend {
    pub fn new(config: CdpConfig) -> Self {
        Self {
            config,
            session: None,
            chrome_process: None,
        }
    }

    async fn ensure_connected(&mut self) -> Result<()> {
        if self.session.is_some() {
            return Ok(());
        }

        let endpoint = self.resolve_endpoint().await?;
        let ws_url = if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
            endpoint
        } else {
            get_page_ws_url(&endpoint)
                .await
                .with_context(|| format!("Failed to get page WebSocket URL from {}", endpoint))?
        };

        debug!(ws_url = %ws_url, "connecting to CDP");
        let (ws, _) = connect_async(&ws_url)
            .await
            .with_context(|| format!("Failed to connect CDP WebSocket at {}", ws_url))?;

        let mut session = CdpSession { ws, next_id: 1 };
        session
            .call("Page.enable", serde_json::json!({}))
            .await
            .context("Page.enable failed")?;
        session
            .call("Runtime.enable", serde_json::json!({}))
            .await
            .context("Runtime.enable failed")?;

        self.session = Some(session);
        Ok(())
    }

    async fn resolve_endpoint(&mut self) -> Result<String> {
        match self.config.mode {
            CdpMode::ManagedLocal => self.launch_managed_chrome().await,
            CdpMode::AttachExisting => Ok(self
                .config
                .cdp_url
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:9222".to_string())),
            CdpMode::RemoteCdp => self
                .config
                .cdp_url
                .clone()
                .ok_or_else(|| anyhow!("RemoteCdp mode requires a cdp_url")),
        }
    }

    async fn launch_managed_chrome(&mut self) -> Result<String> {
        let binary = resolve_chrome_binary(self.config.browser.as_deref()).ok_or_else(|| {
            anyhow!(
                "Chrome/Chromium not found. Install Google Chrome or set the browser path in config."
            )
        })?;

        let port: u16 = 9222;
        let user_data_dir = self.config.user_data_dir.clone().unwrap_or_else(|| {
            std::env::temp_dir()
                .join("rove-browser")
                .to_string_lossy()
                .into_owned()
        });

        let mut args = vec![
            format!("--remote-debugging-port={}", port),
            "--headless=new".to_string(),
            "--no-sandbox".to_string(),
            "--disable-gpu".to_string(),
            "--disable-dev-shm-usage".to_string(),
            format!("--user-data-dir={}", user_data_dir),
        ];
        if let Some(url) = &self.config.startup_url {
            args.push(url.clone());
        }

        info!(binary = %binary.display(), args = ?args, "launching managed Chrome");

        let child = tokio::process::Command::new(&binary)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to spawn Chrome at {}", binary.display()))?;
        self.chrome_process = Some(child);

        let base_url = format!("http://127.0.0.1:{}", port);
        let client = reqwest::Client::new();
        for _ in 0..25 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if client
                .get(format!("{}/json/version", base_url))
                .send()
                .await
                .is_ok()
            {
                info!("Chrome CDP ready at {}", base_url);
                return Ok(base_url);
            }
        }

        Err(anyhow!(
            "Chrome did not respond on port {} within 5 seconds",
            port
        ))
    }

    /// Navigate to `url` and return the page title (inherent method).
    pub(crate) async fn navigate_impl(&mut self, url: &str) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();

        session
            .call("Page.navigate", serde_json::json!({"url": url}))
            .await
            .with_context(|| format!("Page.navigate failed for {}", url))?;

        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let state = session
                .call(
                    "Runtime.evaluate",
                    serde_json::json!({"expression": "document.readyState", "returnByValue": true}),
                )
                .await
                .unwrap_or_default();
            if ready_state_value(&state) == Some("complete") {
                break;
            }
        }

        let title_res = session
            .call(
                "Runtime.evaluate",
                serde_json::json!({"expression": "document.title", "returnByValue": true}),
            )
            .await
            .unwrap_or_default();
        let title = extract_string_value(&title_res)
            .unwrap_or("(no title)")
            .to_string();

        debug!(url = %url, title = %title, "navigation complete");
        Ok(format!("Navigated to: {url}\nTitle: {title}"))
    }

    pub(crate) async fn page_text_impl(&mut self) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();

        let res = session
            .call(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": "document.body ? document.body.innerText : ''",
                    "returnByValue": true
                }),
            )
            .await?;

        let text = extract_string_value(&res).unwrap_or("").to_string();
        if text.is_empty() {
            return Ok("Page has no visible text content.".to_string());
        }
        const MAX: usize = 8_000;
        if text.len() > MAX {
            Ok(format!(
                "{}\n[truncated — {} chars total]",
                &text[..MAX],
                text.len()
            ))
        } else {
            Ok(text)
        }
    }

    pub(crate) async fn click_impl(&mut self, selector: &str) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();

        let selector_json = serde_json::to_string(selector).unwrap_or_default();
        let expr = format!(
            r#"(function(){{
                const sel = {selector_json};
                const el = document.querySelector(sel);
                if (!el) return 'ERROR: no element matches selector: ' + sel;
                el.click();
                const label = el.textContent.trim().slice(0, 60) || el.tagName;
                return 'Clicked: ' + label;
            }})()"#
        );

        let res = session
            .call(
                "Runtime.evaluate",
                serde_json::json!({"expression": expr, "returnByValue": true}),
            )
            .await?;

        Ok(extract_string_value(&res)
            .unwrap_or("Click executed")
            .to_string())
    }

    pub(crate) async fn fill_field_impl(&mut self, selector: &str, value: &str) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();

        let selector_json = serde_json::to_string(selector).unwrap_or_default();
        let value_json = serde_json::to_string(value).unwrap_or_default();
        let expr = format!(
            r#"(function(){{
                const sel = {selector_json};
                const val = {value_json};
                const el = document.querySelector(sel);
                if (!el) return 'ERROR: no element matches selector: ' + sel;
                el.focus();
                el.value = val;
                el.dispatchEvent(new Event('input', {{bubbles: true}}));
                el.dispatchEvent(new Event('change', {{bubbles: true}}));
                return 'Filled ' + (el.name || el.id || el.tagName);
            }})()"#
        );

        let res = session
            .call(
                "Runtime.evaluate",
                serde_json::json!({"expression": expr, "returnByValue": true}),
            )
            .await?;

        Ok(extract_string_value(&res)
            .unwrap_or("Field filled")
            .to_string())
    }

    /// Execute JavaScript and return the result as JSON
    async fn execute_js(&mut self, script: &str) -> Result<serde_json::Value> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();

        let res = session
            .call(
                "Runtime.evaluate",
                serde_json::json!({
                    "expression": script,
                    "returnByValue": true
                }),
            )
            .await?;

        // CDP returns {type: "...", value: ...} after call() extracts "result"
        if let Some(exception) = res.get("exceptionDetails") {
            return Err(anyhow!("JavaScript error: {}", exception));
        }

        Ok(res.get("value").cloned().unwrap_or(serde_json::Value::Null))
    }

    /// Discover all forms and their fields
    pub(crate) async fn inspect_form_impl(&mut self) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();

        let expr = r#"(function(){
            const forms = Array.from(document.querySelectorAll('form'));
            if (forms.length === 0) return '[]';
            
            const result = forms.map((form, idx) => ({
                index: idx,
                action: form.action || '',
                method: form.method || 'GET',
                fields: Array.from(form.querySelectorAll('input, textarea, select')).map(field => ({
                    name: field.name || '',
                    type: field.type || 'text',
                    label: (field.labels && field.labels[0] ? field.labels[0].textContent.trim() : '') || field.placeholder || '',
                    required: field.required
                }))
            }));
            return JSON.stringify(result);
        })()"#;

        let res = session
            .call(
                "Runtime.evaluate",
                serde_json::json!({"expression": expr, "returnByValue": true}),
            )
            .await?;

        Ok(extract_string_value(&res).unwrap_or("[]").to_string())
    }

    /// Fill form intelligently by matching data to fields
    pub(crate) async fn fill_form_smart_impl(
        &mut self,
        data: serde_json::Value,
        submit: bool,
    ) -> Result<String> {
        self.ensure_connected().await?;

        // Discover forms using direct CDP call
        let discover_expr = r#"(function(){
            const forms = Array.from(document.querySelectorAll('form'));
            if (forms.length === 0) return '[]';
            return JSON.stringify(forms.map(form => ({
                fields: Array.from(form.querySelectorAll('input, textarea, select')).map(field => ({
                    name: field.name || '',
                    type: field.type || 'text',
                    label: (field.labels && field.labels[0] ? field.labels[0].textContent.trim() : '') || field.placeholder || '',
                    id: field.id || ''
                }))
            })));
        })()"#;

        let res = self.session.as_mut().unwrap()
            .call(
                "Runtime.evaluate",
                serde_json::json!({"expression": discover_expr, "returnByValue": true}),
            )
            .await?;

        let forms_str = extract_string_value(&res).unwrap_or("[]");
        let forms: serde_json::Value = serde_json::from_str(forms_str)?;
        let forms_array = forms.as_array().ok_or_else(|| anyhow!("No forms found"))?;
        if forms_array.is_empty() {
            return Ok(serde_json::json!({"filled": [], "skipped": [], "submitted": false}).to_string());
        }

        let form = &forms_array[0];
        let fields = form["fields"]
            .as_array()
            .ok_or_else(|| anyhow!("Invalid form structure"))?;

        let data_obj = data
            .as_object()
            .ok_or_else(|| anyhow!("Data must be an object"))?;

        let mut filled = Vec::new();
        let mut skipped = Vec::new();

        // Match and fill fields
        for (key, value) in data_obj {
            let value_string = value
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| value.to_string());
            let key_lower = key.to_lowercase();

            // Find matching field
            let mut found = false;
            for field in fields {
                let name = field["name"].as_str().unwrap_or("");
                let label = field["label"].as_str().unwrap_or("");
                let id = field["id"].as_str().unwrap_or("");
                let tag = field["tagName"].as_str().unwrap_or("");

                // Match by name, label, or id (case-insensitive)
                if name.to_lowercase() == key_lower
                    || label.to_lowercase().contains(&key_lower)
                    || id.to_lowercase() == key_lower
                {
                    // Build selector
                    let selector = if !name.is_empty() {
                        format!("{}[name='{}']", tag, name)
                    } else if !id.is_empty() {
                        format!("#{}", id)
                    } else {
                        continue;
                    };

                    // Fill the field
                    match self.fill_field_impl(&selector, &value_string).await {
                        Ok(_) => {
                            filled.push(key.clone());
                            found = true;
                            break;
                        }
                        Err(_) => continue,
                    }
                }
            }

            if !found {
                skipped.push(key.clone());
            }
        }

        // Submit if requested
        let submitted = if submit {
            let submit_expr = r#"(function(){
                const btn = document.querySelector('button[type="submit"], input[type="submit"]');
                if (btn) { btn.click(); return 'submitted'; }
                return 'no button';
            })()"#;
            
            match self.session.as_mut().unwrap().call("Runtime.evaluate", serde_json::json!({"expression": submit_expr, "returnByValue": true})).await {
                Ok(_) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    true
                }
                Err(_) => false,
            }
        } else {
            false
        };

        Ok(serde_json::json!({
            "filled": filled,
            "skipped": skipped,
            "submitted": submitted
        }).to_string())
    }

    /// Extract specific data by semantic keys
    pub(crate) async fn extract_semantic_data_impl(&mut self, keys: Vec<String>) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();
        
        let keys_json = serde_json::to_string(&keys)?;
        let expr = format!(r#"(function(){{
            const keys = {keys_json};
            const result = {{}};
            
            keys.forEach(key => {{
                const selectors = [
                    `[class*="${{key}}" i]`,
                    `[id*="${{key}}" i]`,
                    `[itemprop="${{key}}"]`,
                    `[aria-label*="${{key}}" i]`,
                    `[data-testid*="${{key}}" i]`,
                    `[name*="${{key}}" i]`
                ];
                
                for (const sel of selectors) {{
                    const el = document.querySelector(sel);
                    if (el && el.textContent.trim()) {{
                        result[key] = el.textContent.trim().slice(0, 200);
                        break;
                    }}
                }}
            }});
            
            return JSON.stringify(result);
        }})()"#);
        
        let res = session
            .call(
                "Runtime.evaluate",
                serde_json::json!({"expression": expr, "returnByValue": true}),
            )
            .await?;
        
        Ok(extract_string_value(&res).unwrap_or("{}").to_string())
    }

    /// Get semantic page structure


    /// Wait for network to be idle (no pending requests)
    pub(crate) async fn wait_for_network_idle_impl(&mut self, timeout_ms: u64) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();
        
        let start = std::time::Instant::now();
        let timeout = Duration::from_millis(timeout_ms);
        let idle_threshold = Duration::from_millis(500);
        let mut last_activity = std::time::Instant::now();
        
        loop {
            if start.elapsed() > timeout {
                return Ok("timeout".to_string());
            }
            
            if last_activity.elapsed() > idle_threshold {
                return Ok("idle".to_string());
            }
            
            tokio::time::sleep(Duration::from_millis(100)).await;
            
            // Check pending requests
            let expr = r#"(function(){
                return performance.getEntriesByType('resource')
                    .filter(r => r.responseEnd === 0).length;
            })()"#;
            
            let res = session.call("Runtime.evaluate",
                serde_json::json!({"expression": expr, "returnByValue": true})).await?;
            
            if let Some(count) = res.get("value").and_then(|v| v.as_u64()) {
                if count > 0 {
                    last_activity = std::time::Instant::now();
                }
            }
        }
    }

    /// Get only interactive elements with temporary IDs
    pub(crate) async fn get_interactive_elements_impl(&mut self) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();
        
        let expr = r#"(function(){
            const elements = [];
            let id = 1;
            
            const selectors = 'a, button, input, select, textarea, [onclick], [role="button"]';
            document.querySelectorAll(selectors).forEach(el => {
                const rect = el.getBoundingClientRect();
                const style = window.getComputedStyle(el);
                
                if (style.display !== 'none' && 
                    style.visibility !== 'hidden' &&
                    rect.width > 0 && rect.height > 0) {
                    
                    el.setAttribute('data-rove-id', id);
                    
                    elements.push({
                        id: id++,
                        tag: el.tagName.toLowerCase(),
                        type: el.type || '',
                        text: (el.textContent || el.value || el.placeholder || '').trim().slice(0, 50),
                        name: el.name || '',
                        ariaLabel: el.getAttribute('aria-label') || ''
                    });
                }
            });
            
            return JSON.stringify(elements);
        })()"#;
        
        let res = session.call("Runtime.evaluate",
            serde_json::json!({"expression": expr, "returnByValue": true})).await?;
        
        Ok(extract_string_value(&res).unwrap_or("[]").to_string())
    }

    /// Click element by temporary ID
    pub(crate) async fn click_by_id_impl(&mut self, id: u32) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();
        
        let expr = format!(r#"(function(){{
            const el = document.querySelector('[data-rove-id="{}"]');
            if (!el) return 'ERROR: no element with id {}';
            el.click();
            return 'Clicked: ' + (el.textContent || el.value || el.tagName).trim().slice(0, 60);
        }})()"#, id, id);
        
        let res = session.call("Runtime.evaluate",
            serde_json::json!({"expression": expr, "returnByValue": true})).await?;
        
        Ok(extract_string_value(&res).unwrap_or("Click executed").to_string())
    }


    /// Check for blockers (Cloudflare, reCAPTCHA, cookie banners, 404s)
    pub(crate) async fn check_blockers_impl(&mut self) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();
        
        let expr = r#"(function(){
            const blockers = {
                cloudflare: !!document.querySelector('#challenge-form, .cf-browser-verification, [id*="cf-challenge" i]'),
                recaptcha: !!document.querySelector('.g-recaptcha, #recaptcha, iframe[src*="recaptcha"]'),
                cookieBanner: !!document.querySelector('[class*="cookie" i][class*="banner" i], [id*="cookie" i][id*="consent" i]'),
                error404: document.title.toLowerCase().includes('404') || 
                         document.title.toLowerCase().includes('not found') ||
                         document.body.textContent.includes('Page Not Found'),
                accessDenied: document.body.textContent.includes('Access Denied') ||
                              document.body.textContent.includes('403 Forbidden')
            };
            
            const blocked = Object.entries(blockers).filter(([k, v]) => v).map(([k]) => k);
            
            return JSON.stringify({
                blocked: blocked.length > 0,
                blockers: blocked,
                canProceed: blocked.length === 0
            });
        })()"#;
        
        let res = session.call("Runtime.evaluate",
            serde_json::json!({"expression": expr, "returnByValue": true})).await?;
        
        Ok(extract_string_value(&res).unwrap_or("{}").to_string())
    }

    /// Get element diff - returns only changed elements
    pub(crate) async fn get_element_diff_impl(&mut self, selector: String) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();
        
        let expr = format!(r#"(function(){{
            const sel = '{}';
            const elements = Array.from(document.querySelectorAll(sel));
            const snapshot = elements.map(el => ({{
                tag: el.tagName.toLowerCase(),
                text: el.textContent.trim().substring(0, 100),
                attrs: Array.from(el.attributes).reduce((acc, attr) => {{
                    acc[attr.name] = attr.value;
                    return acc;
                }}, {{}})
            }}));
            
            if (!window.__cdp_snapshots) window.__cdp_snapshots = {{}};
            const prev = window.__cdp_snapshots[sel] || [];
            window.__cdp_snapshots[sel] = snapshot;
            
            const changed = snapshot.filter((curr, i) => {{
                const old = prev[i];
                return !old || JSON.stringify(curr) !== JSON.stringify(old);
            }});
            
            return JSON.stringify({{
                total: snapshot.length,
                changed: changed.length,
                elements: changed
            }});
        }})()"#, selector);
        
        let res = session.call("Runtime.evaluate",
            serde_json::json!({"expression": expr, "returnByValue": true})).await?;
        
        Ok(extract_string_value(&res).unwrap_or("{}").to_string())
    }

    /// Extract HTML table as structured JSON
    pub(crate) async fn extract_table_to_json_impl(&mut self, selector: Option<String>) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();
        
        let sel = selector.unwrap_or_else(|| "table".to_string());
        let expr = format!(r#"(function(){{
            const table = document.querySelector('{}');
            if (!table) return JSON.stringify({{"error": "Table not found"}});
            
            const headers = Array.from(table.querySelectorAll('thead th, thead td'))
                .map(th => th.textContent.trim());
            
            const rows = Array.from(table.querySelectorAll('tbody tr')).map(tr => {{
                const cells = Array.from(tr.querySelectorAll('td, th'));
                const row = {{}};
                cells.forEach((cell, i) => {{
                    const key = headers[i] || `col_${{i}}`;
                    row[key] = cell.textContent.trim();
                }});
                return row;
            }});
            
            return JSON.stringify({{
                headers: headers,
                rows: rows,
                count: rows.length
            }});
        }})()"#, sel);
        
        let res = session.call("Runtime.evaluate",
            serde_json::json!({"expression": expr, "returnByValue": true})).await?;
        
        Ok(extract_string_value(&res).unwrap_or("{}").to_string())
    }


    /// Save session state (cookies + localStorage)
    pub(crate) async fn save_session_state_impl(&mut self) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();
        
        let cookies = session.call("Network.getAllCookies", serde_json::json!({})).await?;
        
        let expr = r#"(function(){
            const storage = {};
            for (let i = 0; i < localStorage.length; i++) {
                const key = localStorage.key(i);
                storage[key] = localStorage.getItem(key);
            }
            return JSON.stringify(storage);
        })()"#;
        
        let res = session.call("Runtime.evaluate",
            serde_json::json!({"expression": expr, "returnByValue": true})).await?;
        let storage = extract_string_value(&res).unwrap_or("{}");
        
        Ok(serde_json::json!({
            "cookies": cookies.get("cookies"),
            "localStorage": serde_json::from_str::<serde_json::Value>(storage).ok()
        }).to_string())
    }

    /// Restore session state (cookies + localStorage)
    pub(crate) async fn restore_session_state_impl(&mut self, state: String) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();
        
        let state_obj: serde_json::Value = serde_json::from_str(&state)
            .map_err(|e| anyhow::anyhow!("Invalid state JSON: {}", e))?;
        
        if let Some(cookies) = state_obj.get("cookies").and_then(|c| c.as_array()) {
            for cookie in cookies {
                let _ = session.call("Network.setCookie", cookie.clone()).await;
            }
        }
        
        if let Some(storage) = state_obj.get("localStorage").and_then(|s| s.as_object()) {
            for (key, value) in storage {
                let val_str = value.as_str().unwrap_or("");
                let expr = format!(r#"localStorage.setItem('{}', '{}')"#, 
                    key.replace('\'', "\\'"), val_str.replace('\'', "\\'"));
                let _ = session.call("Runtime.evaluate",
                    serde_json::json!({"expression": expr})).await;
            }
        }
        
        Ok(r#"{"restored": true}"#.to_string())
    }

    /// Intercept API responses (XHR/Fetch)
    pub(crate) async fn intercept_api_response_impl(&mut self, url_pattern: String) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();
        
        session.call("Network.enable", serde_json::json!({})).await?;
        
        let expr = format!(r#"(function(){{
            const pattern = '{}';
            if (!window.__cdp_intercepted) window.__cdp_intercepted = [];
            
            const origFetch = window.fetch;
            window.fetch = function(...args) {{
                return origFetch.apply(this, args).then(response => {{
                    const url = args[0];
                    if (url.includes(pattern)) {{
                        return response.clone().text().then(body => {{
                            window.__cdp_intercepted.push({{
                                url: url,
                                status: response.status,
                                body: body.substring(0, 5000)
                            }});
                            return response;
                        }});
                    }}
                    return response;
                }});
            }};
            
            return JSON.stringify({{"intercepting": pattern}});
        }})()"#, url_pattern);
        
        let res = session.call("Runtime.evaluate",
            serde_json::json!({"expression": expr, "returnByValue": true})).await?;
        
        Ok(extract_string_value(&res).unwrap_or("{}").to_string())
    }

    /// Get intercepted API responses
    pub(crate) async fn get_intercepted_responses_impl(&mut self) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();
        
        let expr = r#"(function(){
            const responses = window.__cdp_intercepted || [];
            window.__cdp_intercepted = [];
            return JSON.stringify({
                count: responses.length,
                responses: responses
            });
        })()"#;
        
        let res = session.call("Runtime.evaluate",
            serde_json::json!({"expression": expr, "returnByValue": true})).await?;
        
        Ok(extract_string_value(&res).unwrap_or("{}").to_string())
    }

    pub(crate) async fn get_page_structure_impl(&mut self) -> Result<String> {
        self.ensure_connected().await?;
        let session = self.session.as_mut().unwrap();

        let expr = r#"(function(){
            const result = {
                title: document.title || '',
                url: window.location.href,
                headings: Array.from(document.querySelectorAll('h1, h2, h3')).map(h => h.textContent.trim()).slice(0, 10),
                forms: document.querySelectorAll('form').length,
                inputs: document.querySelectorAll('input, textarea, select').length,
                buttons: Array.from(document.querySelectorAll('button, input[type="submit"]')).map(b => b.textContent.trim() || b.value).slice(0, 10),
                links: Array.from(document.querySelectorAll('a[href]')).slice(0, 20).map(a => ({
                    text: a.textContent.trim().slice(0, 50),
                    href: a.href
                }))
            };
            return JSON.stringify(result);
        })()"#;

        let res = session
            .call(
                "Runtime.evaluate",
                serde_json::json!({"expression": expr, "returnByValue": true}),
            )
            .await?;

        Ok(extract_string_value(&res).unwrap_or("{}").to_string())
    }
}

impl Drop for CdpBackend {
    fn drop(&mut self) {
        if let Some(mut child) = self.chrome_process.take() {
            if let Err(e) = child.start_kill() {
                warn!(error = %e, "Failed to kill managed Chrome on drop");
            }
        }
    }
}

#[async_trait]
impl BrowserBackend for CdpBackend {
    async fn navigate(&mut self, url: &str) -> Result<String, EngineError> {
        self.navigate_impl(url)
            .await
            .map_err(|e| EngineError::ToolError(e.to_string()))
    }

    async fn page_text(&mut self) -> Result<String, EngineError> {
        self.page_text_impl()
            .await
            .map_err(|e| EngineError::ToolError(e.to_string()))
    }

    async fn click(&mut self, selector: &str) -> Result<String, EngineError> {
        self.click_impl(selector)
            .await
            .map_err(|e| EngineError::ToolError(e.to_string()))
    }

    async fn fill_field(&mut self, selector: &str, value: &str) -> Result<String, EngineError> {
        self.fill_field_impl(selector, value)
            .await
            .map_err(|e| EngineError::ToolError(e.to_string()))
    }

    fn backend_name(&self) -> &str {
        "Chrome CDP"
    }

    fn is_connected(&self) -> bool {
        self.session.is_some()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn get_page_ws_url(http_base: &str) -> Result<String> {
    let base = http_base
        .replace("ws://", "http://")
        .replace("wss://", "https://")
        .trim_end_matches('/')
        .to_string();

    let client = reqwest::Client::new();

    let list_url = format!("{}/json/list", base);
    if let Ok(resp) = client.get(&list_url).send().await {
        if let Ok(targets) = resp.json::<serde_json::Value>().await {
            if let Some(arr) = targets.as_array() {
                for target in arr {
                    if target.get("type").and_then(|v| v.as_str()) == Some("page") {
                        if let Some(ws) =
                            target.get("webSocketDebuggerUrl").and_then(|v| v.as_str())
                        {
                            return Ok(ws.to_string());
                        }
                    }
                }
            }
        }
    }

    let new_url = format!("{}/json/new", base);
    let tab: serde_json::Value = client
        .get(&new_url)
        .send()
        .await
        .with_context(|| format!("GET {} failed — is Chrome running?", new_url))?
        .json()
        .await
        .context("Invalid /json/new response")?;

    tab.get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("webSocketDebuggerUrl missing in /json/new response"))
}

/// Find a Chrome or Chromium binary.
pub fn resolve_chrome_binary(override_path: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = override_path {
        let p = PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }

    #[cfg(target_os = "macos")]
    {
        let candidates = [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        ];
        for path in &candidates {
            let p = PathBuf::from(path);
            if p.exists() {
                return Some(p);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let candidates = [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium-browser",
            "/usr/bin/chromium",
            "/usr/local/bin/google-chrome",
            "/snap/bin/chromium",
        ];
        for path in &candidates {
            let p = PathBuf::from(path);
            if p.exists() {
                return Some(p);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let candidates = [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ];
        for path in &candidates {
            let p = PathBuf::from(path);
            if p.exists() {
                return Some(p);
            }
        }
    }

    None
}

fn extract_string_value(res: &serde_json::Value) -> Option<&str> {
    res.get("result")?.get("value")?.as_str()
}

fn ready_state_value(res: &serde_json::Value) -> Option<&str> {
    extract_string_value(res)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

