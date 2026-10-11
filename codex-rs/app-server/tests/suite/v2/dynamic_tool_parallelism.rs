use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::to_response;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

#[tokio::test]
async fn explicit_parallel_tools_reach_host_before_either_response() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(vec![
        responses::sse(vec![
            responses::ev_response_created("parallel-1"),
            responses::ev_function_call("one", "host_read", "{}"),
            responses::ev_function_call("two", "host_read", "{}"),
            responses::ev_completed("parallel-1"),
        ]),
        create_final_assistant_message_sse_response("Done")?,
    ])
    .await;
    let home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(home.path())?;
    let mut host = TestAppServer::builder()
        .with_codex_home(home.path())
        .build()
        .await?;
    let limit = Duration::from_secs(60);
    timeout(limit, host.initialize()).await??;
    // Deserialize the public wire shape rather than bypassing it with an internal handler.
    let params: ThreadStartParams = serde_json::from_value(json!({
        "dynamicTools": [{
            "type": "function", "name": "host_read", "description": "Host bounded read",
            "inputSchema": {"type": "object", "properties": {}},
            "supportsParallelToolCalls": true
        }]
    }))?;
    let request = host.send_thread_start_request_with_auto_env(params).await?;
    let response = timeout(
        limit,
        host.read_stream_until_response_message(RequestId::Integer(request)),
    )
    .await??;
    let thread = to_response::<ThreadStartResponse>(response)?.thread;
    let request = host
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id,
            input: vec![UserInput::Text {
                text: "Read both".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        limit,
        host.read_stream_until_response_message(RequestId::Integer(request)),
    )
    .await??;
    let mut pending = Vec::new();
    for _ in 0..2 {
        let ServerRequest::DynamicToolCall { request_id, params } =
            timeout(limit, host.read_stream_until_request_message()).await??
        else {
            panic!("expected dynamic tool call")
        };
        pending.push((request_id, params.call_id));
    }
    let mut ids = pending.iter().map(|(_, id)| id.clone()).collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, vec!["one", "two"]);
    // Reply in reverse arrival order; each pending call must retain its own result.
    for (id, call) in pending.into_iter().rev() {
        host.send_response(
            id,
            json!({"success": true, "contentItems": [{"type": "inputText", "text": call}]}),
        )
        .await?;
    }
    timeout(
        limit,
        host.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    Ok(())
}
