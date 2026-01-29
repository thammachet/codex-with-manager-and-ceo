use std::borrow::Cow;
use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::ServiceExt;
use rmcp::handler::server::ServerHandler;
use rmcp::model::CallToolRequestParam;
use rmcp::model::CallToolResult;
use rmcp::model::CreateElicitationRequestParam;
use rmcp::model::ElicitationAction;
use rmcp::model::ElicitationSchema;
use rmcp::model::JsonObject;
use rmcp::model::ListToolsResult;
use rmcp::model::PaginatedRequestParam;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerInfo;
use rmcp::model::Tool;
use serde_json::json;
use tokio::task;

#[derive(Clone)]
struct ElicitationServer {
    tools: Arc<Vec<Tool>>,
}

fn stdio() -> (tokio::io::Stdin, tokio::io::Stdout) {
    (tokio::io::stdin(), tokio::io::stdout())
}

impl ElicitationServer {
    fn new() -> Self {
        Self {
            tools: Arc::new(vec![Self::approval_tool()]),
        }
    }

    fn approval_tool() -> Tool {
        #[expect(clippy::expect_used)]
        let schema: JsonObject = serde_json::from_value(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }))
        .expect("approval tool schema should deserialize");

        Tool::new(
            Cow::Borrowed("needs_approval"),
            Cow::Borrowed("Triggers an MCP elicitation request."),
            Arc::new(schema),
        )
    }
}

impl ServerHandler for ElicitationServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
            ..ServerInfo::default()
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        let tools = self.tools.clone();
        async move {
            Ok(ListToolsResult {
                tools: (*tools).clone(),
                next_cursor: None,
                meta: None,
            })
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "needs_approval" => {
                let schema = ElicitationSchema::builder()
                    .title("Approval request")
                    .build()
                    .map_err(|err| {
                        McpError::internal_error(
                            format!("failed to build elicitation schema: {err}"),
                            None,
                        )
                    })?;

                let elicitation = context
                    .peer
                    .create_elicitation(CreateElicitationRequestParam {
                        message: "rmcp test server needs approval".to_string(),
                        requested_schema: schema,
                    })
                    .await
                    .map_err(|err| McpError::internal_error(err.to_string(), None))?;

                match elicitation.action {
                    ElicitationAction::Accept => Ok(CallToolResult {
                        content: Vec::new(),
                        structured_content: Some(json!({"approved": true})),
                        is_error: Some(false),
                        meta: None,
                    }),
                    ElicitationAction::Decline | ElicitationAction::Cancel => Ok(CallToolResult {
                        content: Vec::new(),
                        structured_content: Some(json!({"approved": false})),
                        is_error: Some(true),
                        meta: None,
                    }),
                }
            }
            other => Err(McpError::invalid_params(
                format!("unknown tool: {other}"),
                None,
            )),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("starting rmcp test elicitation server");
    let service = ElicitationServer::new();
    let running = service.serve(stdio()).await?;
    running.waiting().await?;
    task::yield_now().await;
    Ok(())
}
