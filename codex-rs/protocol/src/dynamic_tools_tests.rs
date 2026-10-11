use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn parallel_opt_in_survives_canonical_legacy_and_saved_tool_round_trips() {
    for legacy in [false, true] {
        for parallel in [None, Some(false), Some(true)] {
            let mut value = json!({
                "name": "host_tool", "description": "Host tool",
                "inputSchema": {"type": "object", "properties": {}},
            });
            if !legacy {
                value["type"] = json!("function");
            }
            if let Some(parallel) = parallel {
                value["supportsParallelToolCalls"] = json!(parallel);
            }
            let tools = normalize_dynamic_tool_specs(vec![value]).unwrap();
            let DynamicToolSpec::Function(tool) = &tools[0] else {
                panic!("expected function")
            };
            assert_eq!(tool.supports_parallel_tool_calls, parallel == Some(true));
            let saved = serde_json::to_value(&tools).unwrap();
            assert_eq!(
                serde_json::from_value::<Vec<DynamicToolSpec>>(saved).unwrap(),
                tools
            );
        }
    }
}

#[test]
fn parallel_opt_in_rejects_non_boolean_values() {
    for legacy in [false, true] {
        let mut value = json!({
            "name": "host_tool", "description": "Host tool",
            "inputSchema": {"type": "object", "properties": {}},
            "supportsParallelToolCalls": "true",
        });
        if !legacy {
            value["type"] = json!("function");
        }
        assert!(normalize_dynamic_tool_specs(vec![value]).is_err());
    }
}
