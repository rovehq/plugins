use extism_pdk::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct ReadReq {
    path: String,
}

#[derive(Deserialize)]
struct WriteReq {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct DeleteReq {
    path: String,
}

#[derive(Serialize)]
struct FsResponse {
    success: bool,
    data: Option<String>,
    error: Option<String>,
}

#[host_fn]
extern "ExtismHost" {
    fn host_read_file(path: String) -> String;
    fn host_write_file(path: String, content: String) -> String;
    fn host_delete_file(path: String) -> String;
}

#[plugin_fn]
pub fn read_file(input: String) -> FnResult<String> {
    let req: ReadReq = serde_json::from_str(&input)?;
    let content = unsafe { host_read_file(req.path)? };
    let res = FsResponse {
        success: true,
        data: Some(content),
        error: None,
    };
    Ok(serde_json::to_string(&res)?)
}

#[plugin_fn]
pub fn write_file(input: String) -> FnResult<String> {
    let req: WriteReq = serde_json::from_str(&input)?;
    let status = unsafe { host_write_file(req.path, req.content)? };
    let res = FsResponse {
        success: status == "ok",
        data: None,
        error: if status != "ok" { Some(status) } else { None },
    };
    Ok(serde_json::to_string(&res)?)
}

#[plugin_fn]
pub fn delete_file(input: String) -> FnResult<String> {
    let req: DeleteReq = serde_json::from_str(&input)?;
    let status = unsafe { host_delete_file(req.path)? };
    let res = FsResponse {
        success: status == "ok",
        data: None,
        error: if status != "ok" { Some(status) } else { None },
    };
    Ok(serde_json::to_string(&res)?)
}
