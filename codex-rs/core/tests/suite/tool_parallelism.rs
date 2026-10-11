#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]

use codex_core::TurnInputRequest;
use core_test_support::test_codex::local_selections;
use std::fs;
use std::time::Duration;
use std::time::Instant;

use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_exec_command_call_with_args;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tokio::sync::oneshot;

async fn run_turn(test: &TestCodex, prompt: &str) -> anyhow::Result<()> {
    let session_model = test.session_configured.model.clone();
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.cwd.path());

    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: prompt.into(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                environments: Some(local_selections(test.config.cwd.clone())),
                approval_policy: Some(AskForApproval::Never),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                collaboration_mode: Some(CollaborationMode {
                    mode: ModeKind::Default,
                    settings: Settings {
                        model: session_model,
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            }),
        )
        .await?;

    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    Ok(())
}

async fn run_turn_and_measure(test: &TestCodex, prompt: &str) -> anyhow::Result<Duration> {
    let start = Instant::now();
    run_turn(test, prompt).await?;
    Ok(start.elapsed())
}

async fn build_codex_with_test_tool(server: &wiremock::MockServer) -> anyhow::Result<TestCodex> {
    let mut builder = test_codex().with_model("test-gpt-5.1-codex");
    builder.build(server).await
}

fn assert_parallel_duration(actual: Duration) {
    // Allow headroom for slow CI scheduling; barrier synchronization already enforces overlap.
    assert!(
        actual < Duration::from_millis(1_600),
        "expected parallel execution to finish quickly, got {actual:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_file_tools_run_in_parallel() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = build_codex_with_test_tool(&server).await?;

    let warmup_args = json!({
        "sleep_after_ms": 10,
        "barrier": {
            "id": "parallel-test-sync-warmup",
            "participants": 2,
            "timeout_ms": 1_000,
        }
    })
    .to_string();

    let parallel_args = json!({
        "sleep_after_ms": 300,
        "barrier": {
            "id": "parallel-test-sync",
            "participants": 2,
            "timeout_ms": 1_000,
        }
    })
    .to_string();

    let warmup_first = sse(vec![
        json!({"type": "response.created", "response": {"id": "resp-warm-1"}}),
        ev_function_call("warm-call-1", "test_sync_tool", &warmup_args),
        ev_function_call("warm-call-2", "test_sync_tool", &warmup_args),
        ev_completed("resp-warm-1"),
    ]);
    let warmup_second = sse(vec![
        ev_assistant_message("warm-msg-1", "warmup complete"),
        ev_completed("resp-warm-2"),
    ]);

    let first_response = sse(vec![
        json!({"type": "response.created", "response": {"id": "resp-1"}}),
        ev_function_call("call-1", "test_sync_tool", &parallel_args),
        ev_function_call("call-2", "test_sync_tool", &parallel_args),
        ev_completed("resp-1"),
    ]);
    let second_response = sse(vec![
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-2"),
    ]);
    mount_sse_sequence(
        &server,
        vec![warmup_first, warmup_second, first_response, second_response],
    )
    .await;

    run_turn(&test, "warm up parallel tool").await?;

    let duration = run_turn_and_measure(&test, "exercise sync tool").await?;
    assert_parallel_duration(duration);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_tools_run_in_parallel() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;

    let shell_args = json!({
        "cmd": "sleep 0.25",
        // Avoid user-specific shell startup cost (e.g. zsh profile scripts) in timing assertions.
        "login": false,
        "yield_time_ms": 1_000,
    });
    let args_one = serde_json::to_string(&shell_args)?;
    let args_two = serde_json::to_string(&shell_args)?;

    let first_response = sse(vec![
        json!({"type": "response.created", "response": {"id": "resp-1"}}),
        ev_function_call("call-1", "exec_command", &args_one),
        ev_function_call("call-2", "exec_command", &args_two),
        ev_completed("resp-1"),
    ]);
    let second_response = sse(vec![
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-2"),
    ]);
    mount_sse_sequence(&server, vec![first_response, second_response]).await;

    let duration = run_turn_and_measure(&test, "run exec_command twice").await?;
    assert_parallel_duration(duration);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mixed_parallel_tools_run_in_parallel() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = build_codex_with_test_tool(&server).await?;

    let sync_args = json!({
        "sleep_after_ms": 300
    })
    .to_string();
    let shell_args = serde_json::to_string(&json!({
        "cmd": "sleep 0.25",
        // Avoid user-specific shell startup cost in timing assertions.
        "login": false,
        "yield_time_ms": 1_000,
    }))?;

    let first_response = sse(vec![
        json!({"type": "response.created", "response": {"id": "resp-1"}}),
        ev_function_call("call-1", "test_sync_tool", &sync_args),
        ev_function_call("call-2", "exec_command", &shell_args),
        ev_completed("resp-1"),
    ]);
    let second_response = sse(vec![
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-2"),
    ]);
    mount_sse_sequence(&server, vec![first_response, second_response]).await;

    let duration = run_turn_and_measure(&test, "mix tools").await?;
    assert_parallel_duration(duration);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_results_grouped() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let test = build_codex_with_test_tool(&server).await?;

    let shell_args = serde_json::to_string(&json!({
        "cmd": "echo 'shell output'",
        "yield_time_ms": 1_000,
    }))?;

    mount_sse_once(
        &server,
        sse(vec![
            json!({"type": "response.created", "response": {"id": "resp-1"}}),
            ev_function_call("call-1", "exec_command", &shell_args),
            ev_function_call("call-2", "exec_command", &shell_args),
            ev_function_call("call-3", "exec_command", &shell_args),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let tool_output_request = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    run_turn(&test, "run shell three times").await?;

    let input = tool_output_request.single_request().input();

    // find all function_call inputs with indexes
    let function_calls = input
        .iter()
        .enumerate()
        .filter(|(_, item)| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .collect::<Vec<_>>();

    let function_call_outputs = input
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
        })
        .collect::<Vec<_>>();

    assert_eq!(function_calls.len(), 3);
    assert_eq!(function_call_outputs.len(), 3);

    for (index, _) in &function_calls {
        for (output_index, _) in &function_call_outputs {
            assert!(
                *index < *output_index,
                "all function calls must come before outputs"
            );
        }
    }

    // output should come in the order of the function calls
    let zipped = function_calls
        .iter()
        .zip(function_call_outputs.iter())
        .collect::<Vec<_>>();
    for (call, output) in zipped {
        assert_eq!(
            call.1.get("call_id").and_then(Value::as_str),
            output.1.get("call_id").and_then(Value::as_str)
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_tools_start_before_response_completed_when_stream_delayed() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let output_file = tempfile::NamedTempFile::new()?;
    let output_path = output_file.path();
    let first_response_id = "resp-1";
    let second_response_id = "resp-2";

    let command = format!(
        "perl -MTime::HiRes -e 'print int(Time::HiRes::time()*1000), \"\\n\"' >> \"{}\"",
        output_path.display()
    );
    // Use a non-login shell to avoid slow, user-specific shell init (e.g. zsh profiles)
    // from making this timing-based test flaky.
    let args = json!({
        "cmd": command,
        "login": false,
        "yield_time_ms": 5_000,
    });

    let first_chunk = sse(vec![
        ev_response_created(first_response_id),
        ev_exec_command_call_with_args("call-1", &args),
        ev_exec_command_call_with_args("call-2", &args),
        ev_exec_command_call_with_args("call-3", &args),
        ev_exec_command_call_with_args("call-4", &args),
    ]);
    let second_chunk = sse(vec![ev_completed(first_response_id)]);
    let follow_up = sse(vec![
        ev_assistant_message("msg-1", "done"),
        ev_completed(second_response_id),
    ]);

    let (first_gate_tx, first_gate_rx) = oneshot::channel();
    let (completion_gate_tx, completion_gate_rx) = oneshot::channel();
    let (follow_up_gate_tx, follow_up_gate_rx) = oneshot::channel();
    let (streaming_server, completion_receivers) = start_streaming_sse_server(vec![
        vec![
            StreamingSseChunk {
                gate: Some(first_gate_rx),
                body: first_chunk,
            },
            StreamingSseChunk {
                gate: Some(completion_gate_rx),
                body: second_chunk,
            },
        ],
        vec![StreamingSseChunk {
            gate: Some(follow_up_gate_rx),
            body: follow_up,
        }],
    ])
    .await;

    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder
        .build_with_streaming_server(&streaming_server)
        .await?;

    let session_model = test.session_configured.model.clone();
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.cwd.path());
    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "stream delayed completion".into(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                environments: Some(local_selections(test.config.cwd.clone())),
                approval_policy: Some(AskForApproval::Never),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                collaboration_mode: Some(CollaborationMode {
                    mode: ModeKind::Default,
                    settings: Settings {
                        model: session_model,
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            }),
        )
        .await?;

    let _ = first_gate_tx.send(());
    let _ = follow_up_gate_tx.send(());

    let timestamps = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let contents = fs::read_to_string(output_path)?;
            let timestamps = contents
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    line.trim()
                        .parse::<i64>()
                        .map_err(|err| anyhow::anyhow!("invalid timestamp {line:?}: {err}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if timestamps.len() == 4 {
                return Ok::<_, anyhow::Error>(timestamps);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;

    let _ = completion_gate_tx.send(());
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let mut completion_iter = completion_receivers.into_iter();
    let completed_at = completion_iter
        .next()
        .expect("completion receiver missing")
        .await
        .expect("completion timestamp missing");
    let count = i64::try_from(timestamps.len()).expect("timestamp count fits in i64");
    assert_eq!(count, 4);

    for timestamp in timestamps {
        assert!(
            timestamp <= completed_at,
            "timestamp {timestamp} should be before or equal to completed {completed_at}"
        );
    }

    streaming_server.shutdown().await;

    Ok(())
}

#[test_case::test_case(false, false; "default tools stay exclusive")]
#[test_case::test_case(true, true; "opted in tools overlap")]
#[test_case::test_case(true, false; "exclusive tool waits for parallel tool")]
#[test_case::test_case(false, true; "parallel tool waits for exclusive tool")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dynamic_tools_respect_explicit_parallel_admission(
    first_parallel: bool,
    second_parallel: bool,
) -> anyhow::Result<()> {
    use codex_core::StartThreadOptions;
    use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
    use codex_protocol::dynamic_tools::DynamicToolResponse;
    use codex_protocol::dynamic_tools::normalize_dynamic_tool_specs;
    use codex_protocol::protocol::Op;

    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("parallel-dynamic-1"),
                ev_function_call("first", "first_tool", "{}"),
                ev_function_call("second", "second_tool", "{}"),
                ev_completed("parallel-dynamic-1"),
            ]),
            sse(vec![
                ev_assistant_message("done", "finished"),
                ev_completed("parallel-dynamic-2"),
            ]),
        ],
    )
    .await;
    let mut builder = test_codex();
    let test = builder.build_with_auto_env(&server).await?;
    let specs = [
        ("first_tool", first_parallel),
        ("second_tool", second_parallel),
    ]
    .into_iter()
    .map(|(name, parallel)| {
        let mut spec = json!({
            "type": "function", "name": name, "description": "Host tool",
            "inputSchema": {"type": "object", "properties": {}},
        });
        if parallel {
            spec["supportsParallelToolCalls"] = json!(true);
        }
        spec
    })
    .collect();
    let thread = test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: normalize_dynamic_tool_specs(specs)?,
            ..StartThreadOptions::new(test.config.clone())
        })
        .await?
        .thread;
    thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Call both tools".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let EventMsg::DynamicToolCallRequest(first) = wait_for_event(&thread, |event| {
        matches!(event, EventMsg::DynamicToolCallRequest(_))
    })
    .await
    else {
        unreachable!("matching event")
    };
    let mut second = if first_parallel && second_parallel {
        // Positive overlap waits for the normal test event timeout; do not use
        // a short scheduling deadline as a performance assertion on loaded CI.
        let EventMsg::DynamicToolCallRequest(second) = wait_for_event(&thread, |event| {
            matches!(event, EventMsg::DynamicToolCallRequest(_))
        })
        .await
        else {
            unreachable!("matching event")
        };
        Some(second)
    } else {
        let next = tokio::time::timeout(
            Duration::from_millis(500),
            wait_for_event(&thread, |event| {
                matches!(event, EventMsg::DynamicToolCallRequest(_))
            }),
        )
        .await;
        assert!(
            next.is_err(),
            "exclusive call must wait for the first response"
        );
        None
    };
    let response = DynamicToolResponse {
        content_items: vec![DynamicToolCallOutputContentItem::InputText {
            text: "ok".to_string(),
        }],
        success: true,
    };
    thread
        .submit(Op::DynamicToolResponse {
            id: first.call_id.clone(),
            response: response.clone(),
        })
        .await?;
    if second.is_none() {
        let EventMsg::DynamicToolCallRequest(request) = wait_for_event(&thread, |event| {
            matches!(event, EventMsg::DynamicToolCallRequest(_))
        })
        .await
        else {
            unreachable!("matching event")
        };
        second = Some(request);
    }
    let second = second.unwrap();
    let mut ids = vec![first.call_id, second.call_id.clone()];
    ids.sort();
    assert_eq!(ids, vec!["first", "second"]);
    thread
        .submit(Op::DynamicToolResponse {
            id: second.call_id,
            response,
        })
        .await?;
    wait_for_event(&thread, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    for tool in requests[0].body_json()["tools"].as_array().unwrap() {
        assert!(
            tool.get("supportsParallelToolCalls").is_none(),
            "host admission is not a model parameter"
        );
    }
    Ok(())
}
