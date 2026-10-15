//! Newline-delimited JSON-RPC 2.0 MCP server on stdio (ported from Switchyard's
//! `switchyard-cli::mcp::server`, same owner; blocking, since every tool is one git call).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

/// MCP protocol revision spoken.
pub const PROTOCOL_VERSION: &str = "2024-11-05";
/// Server name in `initialize`.
pub const SERVER_NAME: &str = "switchhead";
/// Instructions handed to the client on `initialize`.
pub const SERVER_INSTRUCTIONS: &str = include_str!("instructions.md");

/// One JSON-RPC request or notification.
#[derive(Debug, Deserialize)]
pub struct Request {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct Response {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

/// A tool definition.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    /// Name.
    pub name: &'static str,
    /// What it does, for the model.
    pub description: &'static str,
    /// JSON Schema of the arguments.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// A tool result in MCP's content-block shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResult {
    /// Content blocks.
    pub content: Vec<Value>,
    /// Whether the call failed.
    #[serde(rename = "isError")]
    pub is_error: bool,
}

impl ToolResult {
    /// A text result.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: vec![json!({"type": "text", "text": s.into()})],
            is_error: false,
        }
    }

    /// A failed call.
    pub fn error(s: impl Into<String>) -> Self {
        Self {
            content: vec![json!({"type": "text", "text": s.into()})],
            is_error: true,
        }
    }
}

/// What the server serves. Every tool switchhead exposes is read-only.
pub trait ToolHost {
    /// Tool definitions.
    fn tools(&self) -> Vec<ToolDef>;
    /// Run one tool.
    fn call(&mut self, name: &str, args: &Value) -> ToolResult;
}

/// Handle one decoded request. `None` for notifications.
pub fn handle(host: &mut impl ToolHost, req: Request) -> Option<Value> {
    let id = req.id.clone()?;
    let ok = |result: Value| Response {
        jsonrpc: "2.0",
        id: id.clone(),
        result: Some(result),
        error: None,
    };
    let resp = match req.method.as_str() {
        "initialize" => ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
            "instructions": SERVER_INSTRUCTIONS
        })),
        "ping" => ok(json!({})),
        "tools/list" => {
            let tools: Vec<Value> = host
                .tools()
                .into_iter()
                .map(|t| {
                    let mut v = json!(t);
                    v["annotations"] = json!({"readOnlyHint": true, "destructiveHint": false});
                    v
                })
                .collect();
            ok(json!({ "tools": tools }))
        }
        "tools/call" => {
            let result = call_tool(host, &req.params);
            ok(serde_json::to_value(result).unwrap_or(Value::Null))
        }
        other => Response {
            jsonrpc: "2.0",
            id: id.clone(),
            result: None,
            error: Some(RpcError {
                code: -32601,
                message: format!("method not found: {other}"),
            }),
        },
    };
    serde_json::to_value(resp).ok()
}

fn call_tool(host: &mut impl ToolHost, params: &Value) -> ToolResult {
    let Some(name) = params
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
    else {
        return ToolResult::error("tools/call requires a nonempty string name");
    };
    let args = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(a) if a.is_object() => a.clone(),
        Some(_) => return ToolResult::error("tools/call arguments must be an object"),
    };
    if !host.tools().iter().any(|t| t.name == name) {
        return ToolResult::error(format!("unknown tool: {name}"));
    }
    host.call(name, &args)
}

/// Serve until `input` ends.
pub fn serve(
    host: &mut impl ToolHost,
    input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle(host, req),
            Err(e) => serde_json::to_value(Response {
                jsonrpc: "2.0",
                id: Value::Null,
                result: None,
                error: Some(RpcError {
                    code: -32700,
                    message: format!("parse error: {e}"),
                }),
            })
            .ok(),
        };
        if let Some(r) = reply {
            output.write_all(format!("{r}\n").as_bytes())?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl ToolHost for Echo {
        fn tools(&self) -> Vec<ToolDef> {
            vec![ToolDef {
                name: "echo",
                description: "Echo",
                input_schema: json!({"type": "object"}),
            }]
        }
        fn call(&mut self, _: &str, args: &Value) -> ToolResult {
            ToolResult::text(args.to_string())
        }
    }

    fn roundtrip(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve(&mut Echo, input.as_bytes(), &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn initialize_list_call_and_errors() {
        let out = roundtrip(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"a":1}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"drop_table"}}"#,
            "\n",
            "not json\n",
        ));
        assert_eq!(out.len(), 5, "the notification gets no reply");
        assert_eq!(out[0]["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(
            out[1]["result"]["tools"][0]["annotations"]["readOnlyHint"],
            true
        );
        assert_eq!(out[2]["result"]["content"][0]["text"], r#"{"a":1}"#);
        assert_eq!(out[3]["result"]["isError"], true);
        assert_eq!(out[4]["error"]["code"], -32700);
    }
}
