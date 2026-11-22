use serde_json::json;

use super::*;

fn schema(value: Value) -> prost_types::Struct {
    let Value::Object(map) = value else {
        panic!("schemas are objects");
    };
    json_to_struct(map)
}

fn tool(
    name: &str,
    input_schema: Option<prost_types::Struct>,
) -> api::request::mcp_context::McpTool {
    api::request::mcp_context::McpTool {
        name: name.to_owned(),
        description: format!("{name} description"),
        input_schema,
    }
}

fn server(
    id: &str,
    name: &str,
    tools: Vec<api::request::mcp_context::McpTool>,
) -> api::request::mcp_context::McpServer {
    api::request::mcp_context::McpServer {
        id: id.to_owned(),
        name: name.to_owned(),
        tools,
        ..Default::default()
    }
}

fn request(context: api::request::McpContext, supported: &[api::ToolType]) -> api::Request {
    api::Request {
        mcp_context: Some(context),
        settings: Some(api::request::Settings {
            supported_tools: supported.iter().map(|tool| *tool as i32).collect(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn names(tools: &McpTools) -> Vec<String> {
    tools
        .definitions()
        .map(|tool| tool.name.to_string())
        .collect()
}

#[test]
fn tools_are_namespaced_per_server_and_unique() {
    let context = api::request::McpContext {
        servers: vec![
            server(
                "s1",
                "GitHub Enterprise",
                vec![tool("search", None), tool("get.issue", None)],
            ),
            server("s2", "GitHub Enterprise", vec![tool("search", None)]),
            server(
                "s3",
                &"x".repeat(80),
                vec![tool("t", None), tool("t", None)],
            ),
        ],
        ..Default::default()
    };
    let tools = McpTools::from_request(&request(context, &[api::ToolType::CallMcpTool]));
    let names = names(&tools);
    assert_eq!(names[0], "mcp__GitHub_Enterprise__search");
    assert_eq!(names[1], "mcp__GitHub_Enterprise__get_issue");
    assert!(names[2].starts_with("mcp__GitHub_Enterprise__search_"));
    assert_ne!(names[2], names[0]);
    assert!(names[3].len() <= MAX_NAME_LEN && names[4].len() <= MAX_NAME_LEN);
    assert_eq!(names[3].len(), MAX_NAME_LEN);
    assert_eq!(names[4].len(), MAX_NAME_LEN);
    assert_ne!(names[3], names[4]);
}

#[test]
fn colliding_names_survive_servers_being_reordered_or_added() {
    let a = server("s1", "github", vec![tool("search", None)]);
    let b = server("s2", "github", vec![tool("search", None)]);
    let c = server("s3", "github", vec![tool("search", None)]);
    let model_name_for = |servers: Vec<api::request::mcp_context::McpServer>, id: &str| {
        let context = api::request::McpContext {
            servers,
            ..Default::default()
        };
        let tools = McpTools::from_request(&request(context, &[api::ToolType::CallMcpTool]));
        tools
            .tools
            .iter()
            .find(|tool| tool.server_id == id)
            .map(|tool| tool.model_name.clone())
            .unwrap()
    };
    let before = model_name_for(vec![a.clone(), b.clone()], "s2");
    let after = model_name_for(vec![c, a, b], "s2");
    assert_eq!(before, after);
}

#[test]
fn unnamed_servers_fall_back_to_their_id() {
    let context = api::request::McpContext {
        servers: vec![
            server("srv-1", "", vec![tool("t", None)]),
            server("srv-2", "日本", vec![tool("t", None)]),
        ],
        ..Default::default()
    };
    let tools = McpTools::from_request(&request(context, &[api::ToolType::CallMcpTool]));
    assert_eq!(names(&tools), ["mcp__srv-1__t", "mcp__srv-2__t"]);
}

#[test]
fn mcp_names_cannot_shadow_built_in_tools() {
    let built_ins = [
        "run_shell_command",
        "read_files",
        "edit_file",
        "write_file",
        "grep",
        "find_files",
    ];
    let context = api::request::McpContext {
        servers: vec![server(
            "s1",
            "",
            built_ins.iter().map(|name| tool(name, None)).collect(),
        )],
        #[allow(deprecated)]
        tools: built_ins.iter().map(|name| tool(name, None)).collect(),
        ..Default::default()
    };
    let tools = McpTools::from_request(&request(context, &[api::ToolType::CallMcpTool]));
    for name in names(&tools) {
        assert!(name.starts_with(NAME_PREFIX), "{name}");
        assert!(!built_ins.contains(&name.as_str()));
    }
}

#[test]
fn integers_beyond_f64_precision_are_rounded() {
    let Value::Object(args) = json!({ "id": 9_007_199_254_740_993_u64 }) else {
        unreachable!();
    };
    let round_tripped = struct_to_json(json_to_struct(args));
    assert_eq!(round_tripped["id"].as_f64(), Some(9_007_199_254_740_992.0));
}

#[test]
fn no_mcp_tools_without_client_support() {
    let context = api::request::McpContext {
        servers: vec![server("s1", "linear", vec![tool("search", None)])],
        ..Default::default()
    };
    assert!(
        names(&McpTools::from_request(&request(
            context,
            &[api::ToolType::Grep]
        )))
        .is_empty()
    );
}

#[test]
fn schema_and_arguments_round_trip_through_protobuf() {
    let context = api::request::McpContext {
        servers: vec![server(
            "3f1c",
            "linear",
            vec![tool(
                "create_issue",
                Some(schema(json!({
                    "type": "object",
                    "properties": { "count": { "type": "integer", "minimum": 1 } },
                    "required": ["count"]
                }))),
            )],
        )],
        ..Default::default()
    };
    let tools = McpTools::from_request(&request(context, &[api::ToolType::CallMcpTool]));
    let definition = tools.definitions().next().unwrap();
    assert_eq!(
        Value::Object((*definition.input_schema).clone()),
        json!({
            "type": "object",
            "properties": { "count": { "type": "integer", "minimum": 1 } },
            "required": ["count"]
        })
    );
    assert!(definition.description.unwrap().contains("linear"));

    let Value::Object(args) = json!({ "count": 3, "tags": ["a"], "nested": { "ok": true } }) else {
        unreachable!()
    };
    let Some(call::Tool::CallMcpTool(call)) =
        tools.to_warp_tool("mcp__linear__create_issue", Some(&args))
    else {
        panic!("expected an MCP call");
    };
    assert_eq!(call.name, "create_issue");
    assert_eq!(call.server_id, "3f1c");
    assert_eq!(
        Value::Object(struct_to_json(call.args.unwrap())),
        Value::Object(args)
    );
    assert!(tools.to_warp_tool("run_shell_command", None).is_none());
}

#[test]
fn a_missing_schema_is_an_empty_object_schema() {
    let context = api::request::McpContext {
        servers: vec![server("s", "srv", vec![tool("ping", None)])],
        ..Default::default()
    };
    let tools = McpTools::from_request(&request(context, &[api::ToolType::CallMcpTool]));
    assert_eq!(
        tools.definitions().next().unwrap().input_schema.get("type"),
        Some(&json!("object"))
    );
}

#[test]
fn flat_tools_from_older_clients_have_no_server_id() {
    #[allow(deprecated)]
    let context = api::request::McpContext {
        tools: vec![tool("ping", None)],
        ..Default::default()
    };
    let tools = McpTools::from_request(&request(context, &[api::ToolType::CallMcpTool]));
    let Some(call::Tool::CallMcpTool(call)) = tools.to_warp_tool("mcp__ping", None) else {
        panic!("expected an MCP call");
    };
    assert_eq!(call.name, "ping");
    assert!(call.server_id.is_empty());
}

#[test]
fn identity_gets_the_server_name() {
    let mut srv = server("s", "Linear", vec![tool("ping", None)]);
    srv.identity = Some(api::McpServerIdentity {
        integration: api::McpIntegration::Linear as i32,
        ..Default::default()
    });
    let context = api::request::McpContext {
        servers: vec![srv],
        ..Default::default()
    };
    let tools = McpTools::from_request(&request(context, &[api::ToolType::CallMcpTool]));
    let Some(call::Tool::CallMcpTool(call)) = tools.to_warp_tool("mcp__Linear__ping", None) else {
        panic!("expected an MCP call");
    };
    assert_eq!(call.server_identity.unwrap().display_name, "Linear");
}

#[test]
fn results_render_text_resources_and_errors() {
    use api::call_mcp_tool_result::success::result::{Image, Text};
    use api::call_mcp_tool_result::success::{Result as Part, result::Result as P};
    let success = api::CallMcpToolResult {
        result: Some(api::call_mcp_tool_result::Result::Success(
            api::call_mcp_tool_result::Success {
                results: vec![
                    Part {
                        result: Some(P::Text(Text {
                            text: "hello".to_owned(),
                        })),
                    },
                    Part {
                        result: Some(P::Image(Image {
                            data: vec![0; 10],
                            mime_type: "image/png".to_owned(),
                        })),
                    },
                    Part {
                        result: Some(P::Resource(api::McpResourceContent {
                            uri: "file:///a".to_owned(),
                            content_type: Some(api::mcp_resource_content::ContentType::Text(
                                api::mcp_resource_content::Text {
                                    content: "body".to_owned(),
                                    mime_type: String::new(),
                                },
                            )),
                        })),
                    },
                ],
            },
        )),
    };
    let (text, failed) = result_text(&success);
    assert!(!failed);
    assert!(
        text.contains("hello")
            && text.contains("image/png, 10 bytes")
            && text.contains("=== file:///a\nbody")
    );

    let error = api::CallMcpToolResult {
        result: Some(api::call_mcp_tool_result::Result::Error(
            api::call_mcp_tool_result::Error {
                message: "boom".to_owned(),
            },
        )),
    };
    let (text, failed) = result_text(&error);
    assert!(failed && text.contains("boom"));
}
