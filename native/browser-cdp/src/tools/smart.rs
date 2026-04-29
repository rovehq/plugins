//! Smart CDP tools for token-efficient automation

use anyhow::Result;
use serde_json::Value;

use super::session::CdpSession;
use super::utils::extract_string_value;

/// Extract specific data by semantic keys
pub async fn extract_semantic_data(session: &mut CdpSession, keys: Vec<String>) -> Result<String> {
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

/// Discover all forms and their fields
pub async fn inspect_form(session: &mut CdpSession) -> Result<String> {
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

/// Get semantic page structure
pub async fn get_page_structure(session: &mut CdpSession) -> Result<String> {
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

/// Fill form intelligently by matching data to fields
pub async fn fill_form_smart(
    session: &mut CdpSession,
    fill_field: impl Fn(&str, &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>,
    data: Value,
    submit: bool,
) -> Result<String> {
    use std::time::Duration;
    
    // Discover forms
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

    let res = session
        .call(
            "Runtime.evaluate",
            serde_json::json!({"expression": discover_expr, "returnByValue": true}),
        )
        .await?;

    let forms_str = extract_string_value(&res).unwrap_or("[]");
    let forms: Value = serde_json::from_str(forms_str)?;
    let forms_array = forms.as_array().ok_or_else(|| anyhow::anyhow!("No forms found"))?;
    
    if forms_array.is_empty() {
        return Ok(serde_json::json!({"filled": [], "skipped": [], "submitted": false}).to_string());
    }

    let form = &forms_array[0];
    let fields = form["fields"].as_array().ok_or_else(|| anyhow::anyhow!("Invalid form structure"))?;
    let data_obj = data.as_object().ok_or_else(|| anyhow::anyhow!("Data must be an object"))?;
    
    let mut filled = Vec::new();
    let mut skipped = Vec::new();

    // Match and fill fields
    for (key, value) in data_obj {
        let value_string = value.as_str().map(|s| s.to_string()).unwrap_or_else(|| value.to_string());
        let key_lower = key.to_lowercase();
        
        let mut found = false;
        for field in fields {
            let name = field["name"].as_str().unwrap_or("");
            let label = field["label"].as_str().unwrap_or("");
            let id = field["id"].as_str().unwrap_or("");
            
            if name.to_lowercase() == key_lower ||
               label.to_lowercase().contains(&key_lower) ||
               id.to_lowercase() == key_lower {
                
                let selector = if !name.is_empty() {
                    format!("input[name='{}'], textarea[name='{}'], select[name='{}']", name, name, name)
                } else if !id.is_empty() {
                    format!("#{}", id)
                } else {
                    continue;
                };
                
                match fill_field(&selector, &value_string).await {
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
        
        match session.call("Runtime.evaluate", serde_json::json!({"expression": submit_expr, "returnByValue": true})).await {
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
