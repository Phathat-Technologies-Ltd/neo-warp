//! The client's MCP tools, offered to the model alongside the built-in tools. The client lists its
//! active MCP servers and their tools in each request's `mcp_context`; a model call to one becomes
//! a Warp `CallMcpTool`, which the client runs on that server under the user's MCP permissions.

use std::collections::HashSet;
use std::sync::Arc;

use prost_types::value::Kind;
use rmcp::model::Tool;
use serde_json::{Map, Number, Value};
use warp_multi_agent_api as api;
use warp_multi_agent_api::message::tool_call::{self as call};

const NAME_PREFIX: &str = "mcp__";
/// The longest tool name Anthropic and OpenAI accept.
const MAX_NAME_LEN: usize = 64;

struct McpTool {
    model_name: String,
    tool_name: String,
    server_id: String,
    server_identity: Option<api::McpServerIdentity>,
    definition: Tool,
}

/// The MCP tools the client offers in one request, keyed by the name the model sees.
#[derive(Default)]
pub struct McpTools {
    tools: Vec<McpTool>,
}

impl McpTools {
    pub fn from_request(request: &api::Request) -> Self {
        let supported = request.settings.as_ref().is_some_and(|settings| {
            settings
                .supported_tools
                .contains(&(api::ToolType::CallMcpTool as i32))
        });
        match (&request.mcp_context, supported) {
            (Some(context), true) => Self::from_context(context),
            _ => Self::default(),
        }
    }

    fn from_context(context: &api::request::McpContext) -> Self {
        let mut used = HashSet::new();
        let mut tools = Vec::new();
        for server in &context.servers {
            let server_identity = server.identity.clone().map(|mut identity| {
                if identity.display_name.is_empty() {
                    identity.display_name = server.name.clone();
                }
                identity
            });
            let server_label = server_label(&server.name, &server.id);
            for tool in &server.tools {
                let base = format!("{NAME_PREFIX}{server_label}__{}", sanitize(&tool.name));
                let key = format!("{}\0{}", server.id, tool.name);
                tools.push(McpTool::new(
                    unique_name(base, &key, &mut used),
                    tool,
                    Some(&server.name),
                    server.id.clone(),
                    server_identity.clone(),
                ));
            }
        }
        #[allow(deprecated)]
        for tool in &context.tools {
            let base = format!("{NAME_PREFIX}{}", sanitize(&tool.name));
            tools.push(McpTool::new(
                unique_name(base, &tool.name, &mut used),
                tool,
                None,
                String::new(),
                None,
            ));
        }
        Self { tools }
    }

    pub fn definitions(&self) -> impl Iterator<Item = Tool> + '_ {
        self.tools.iter().map(|tool| tool.definition.clone())
    }

    /// The Warp tool for a model call to `name`, or `None` if it isn't one of these MCP tools.
    pub fn to_warp_tool(
        &self,
        name: &str,
        arguments: Option<&Map<String, Value>>,
    ) -> Option<call::Tool> {
        let tool = self.tools.iter().find(|tool| tool.model_name == name)?;
        Some(call::Tool::CallMcpTool(call::CallMcpTool {
            name: tool.tool_name.clone(),
            args: Some(json_to_struct(arguments.cloned().unwrap_or_default())),
            server_id: tool.server_id.clone(),
            server_identity: tool.server_identity.clone(),
        }))
    }
}

impl McpTool {
    fn new(
        model_name: String,
        tool: &api::request::mcp_context::McpTool,
        server_name: Option<&str>,
        server_id: String,
        server_identity: Option<api::McpServerIdentity>,
    ) -> Self {
        let mut schema = tool
            .input_schema
            .clone()
            .map(struct_to_json)
            .unwrap_or_default();
        schema
            .entry("type")
            .or_insert_with(|| Value::String("object".to_owned()));
        let description = match server_name {
            Some(server) => format!("[MCP server {server}] {}", tool.description),
            None => format!("[MCP] {}", tool.description),
        };
        let definition = Tool::new(model_name.clone(), description, Arc::new(schema));
        Self {
            model_name,
            tool_name: tool.name.clone(),
            server_id,
            server_identity,
            definition,
        }
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The sanitized server name, or the server id when the name has no usable characters.
fn server_label(name: &str, id: &str) -> String {
    let label = sanitize(name);
    if label.chars().any(|c| c.is_ascii_alphanumeric()) {
        label
    } else {
        sanitize(id)
    }
}

/// `base` cut to the name limit. On a collision, a suffix derived from `key` (the server id and
/// tool name) rather than from list order, so a tool keeps its name when other servers are added,
/// removed, or reordered between turns and earlier calls in the conversation still resolve.
fn unique_name(base: String, key: &str, used: &mut HashSet<String>) -> String {
    let base: String = base.chars().take(MAX_NAME_LEN).collect();
    let mut name = base.clone();
    let mut attempt = 0u32;
    while used.contains(&name) {
        let suffix = format!("_{:08x}", stable_hash(key, attempt));
        // `base` is ASCII after `sanitize`, so any byte offset is a char boundary.
        let keep = MAX_NAME_LEN.saturating_sub(suffix.len()).min(base.len());
        name = format!("{}{suffix}", &base[..keep]);
        attempt += 1;
    }
    used.insert(name.clone());
    name
}

/// FNV-1a, which unlike `DefaultHasher` is fixed across Rust releases.
fn stable_hash(key: &str, attempt: u32) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in key.bytes().chain(attempt.to_le_bytes()) {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// The text the model sees for an MCP tool result, and whether the call failed.
pub fn result_text(result: &api::CallMcpToolResult) -> (String, bool) {
    use api::call_mcp_tool_result::Result as R;
    use api::call_mcp_tool_result::success::result::Result as Part;
    use api::mcp_resource_content::ContentType;
    match &result.result {
        Some(R::Success(success)) => {
            let parts = success
                .results
                .iter()
                .filter_map(|part| match part.result.as_ref()? {
                    Part::Text(text) => Some(text.text.clone()),
                    Part::Image(image) => Some(format!(
                        "[image ({}, {} bytes) not shown]",
                        image.mime_type,
                        image.data.len()
                    )),
                    Part::Resource(resource) => Some(match &resource.content_type {
                        Some(ContentType::Text(text)) => {
                            format!("=== {}\n{}", resource.uri, text.content)
                        }
                        Some(ContentType::Binary(binary)) => format!(
                            "[resource {} ({}, {} bytes) not shown]",
                            resource.uri,
                            binary.mime_type,
                            binary.data.len()
                        ),
                        None => format!("[resource {}]", resource.uri),
                    }),
                })
                .collect::<Vec<_>>();
            if parts.is_empty() {
                ("The tool returned no content.".to_owned(), false)
            } else {
                (parts.join("\n\n"), false)
            }
        }
        Some(R::Error(error)) => (format!("The MCP tool failed: {}", error.message), true),
        None => ("The MCP tool produced no result.".to_owned(), true),
    }
}

fn struct_to_json(value: prost_types::Struct) -> Map<String, Value> {
    value
        .fields
        .into_iter()
        .map(|(key, value)| (key, value_to_json(value)))
        .collect()
}

fn value_to_json(value: prost_types::Value) -> Value {
    match value.kind {
        None | Some(Kind::NullValue(_)) => Value::Null,
        Some(Kind::BoolValue(b)) => Value::Bool(b),
        Some(Kind::NumberValue(n)) => {
            // Protobuf numbers are doubles; keep integral values integral so schemas like
            // `"minimum": 1` and integer arguments round-trip unchanged.
            if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
                Value::Number(Number::from(n as i64))
            } else {
                Number::from_f64(n).map_or(Value::Null, Value::Number)
            }
        }
        Some(Kind::StringValue(s)) => Value::String(s),
        Some(Kind::ListValue(list)) => {
            Value::Array(list.values.into_iter().map(value_to_json).collect())
        }
        Some(Kind::StructValue(s)) => Value::Object(struct_to_json(s)),
    }
}

fn json_to_struct(value: Map<String, Value>) -> prost_types::Struct {
    prost_types::Struct {
        fields: value
            .into_iter()
            .map(|(key, value)| (key, json_to_value(value)))
            .collect(),
    }
}

/// Numbers become `f64` because that is the only number `prost_types::Value` holds, so integers
/// beyond 2^53 lose precision.
fn json_to_value(value: Value) -> prost_types::Value {
    let kind = match value {
        Value::Null => Kind::NullValue(0),
        Value::Bool(b) => Kind::BoolValue(b),
        Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or_default()),
        Value::String(s) => Kind::StringValue(s),
        Value::Array(values) => Kind::ListValue(prost_types::ListValue {
            values: values.into_iter().map(json_to_value).collect(),
        }),
        Value::Object(map) => Kind::StructValue(json_to_struct(map)),
    };
    prost_types::Value { kind: Some(kind) }
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
