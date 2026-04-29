//! Basic CDP tools

use anyhow::Result;

use super::session::CdpSession;
use super::utils::extract_string_value;

/// Navigate to URL and return page title
pub async fn navigate(session: &mut CdpSession, url: &str) -> Result<String> {
    use std::time::Duration;
    
    session
        .call("Page.navigate", serde_json::json!({"url": url}))
        .await?;

    // Wait for page to load
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let state = session
            .call(
                "Runtime.evaluate",
                serde_json::json!({"expression": "document.readyState", "returnByValue": true}),
            )
            .await
            .unwrap_or_default();
        
        if state.get("value").and_then(|v| v.as_str()) == Some("complete") {
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
    
    let title = extract_string_value(&title_res).unwrap_or("(no title)");
    Ok(format!("Navigated to: {url}\nTitle: {title}"))
}

/// Get visible page text
pub async fn page_text(session: &mut CdpSession) -> Result<String> {
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
        Ok(format!("{}\n[truncated — {} chars total]", &text[..MAX], text.len()))
    } else {
        Ok(text)
    }
}

/// Click element by CSS selector
pub async fn click(session: &mut CdpSession, selector: &str) -> Result<String> {
    let selector_json = serde_json::to_string(selector)?;
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

    Ok(extract_string_value(&res).unwrap_or("Click executed").to_string())
}

/// Fill form field by CSS selector
pub async fn fill_field(session: &mut CdpSession, selector: &str, value: &str) -> Result<String> {
    let selector_json = serde_json::to_string(selector)?;
    let value_json = serde_json::to_string(value)?;
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

    Ok(extract_string_value(&res).unwrap_or("Field filled").to_string())
}
