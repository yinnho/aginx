//! ACP.md 金样本互锁测试（协议立法层）。
//!
//! 解析本仓根 ACP.md §6 内嵌的 `<!-- golden: 名字 -->` 样本，用网关自己的
//! 类型（RelayMessage / Request / PromptParams）反序列化——文档与实现打架
//! 即此测试红。改协议必须同批改 ACP.md + 全部说话端（见 ACP.md §8）。

const DOC: &str = include_str!("../../ACP.md");

/// 按名字提取金样本：标记行的下一个 ```json 围栏。
fn golden(name: &str) -> serde_json::Value {
    let marker = format!("<!-- golden: {name} -->");
    let start = DOC
        .find(&marker)
        .unwrap_or_else(|| panic!("ACP.md 缺金样本标记: {name}"));
    let rest = &DOC[start + marker.len()..];
    let fence = rest
        .find("```json")
        .unwrap_or_else(|| panic!("金样本 {name} 后缺 ```json 围栏"));
    let body = &rest[fence + "```json".len()..];
    let end = body
        .find("```")
        .unwrap_or_else(|| panic!("金样本 {name} 围栏未闭合"));
    serde_json::from_str(body[..end].trim())
        .unwrap_or_else(|e| panic!("金样本 {name} 不是合法 JSON: {e}"))
}

#[test]
fn relay_frames_parse() {
    use crate::relay::RelayMessage;

    let register: RelayMessage =
        serde_json::from_value(golden("relay_register")).expect("register 帧应可解析");
    assert!(matches!(register, RelayMessage::Register { ref id, .. } if id == "qi7o6bj5"));

    let registered: RelayMessage =
        serde_json::from_value(golden("relay_registered")).expect("registered 帧应可解析");
    assert!(matches!(registered, RelayMessage::Registered { .. }));

    let connect: RelayMessage =
        serde_json::from_value(golden("relay_connect")).expect("connect 帧应可解析");
    assert!(matches!(connect, RelayMessage::Connect { ref target, .. } if target == "qi7o6bj5"));

    let connected: RelayMessage =
        serde_json::from_value(golden("relay_connected")).expect("connected 帧应可解析");
    assert!(matches!(connected, RelayMessage::Connected { .. }));

    // relay→网关方向的 data 帧必须能被网关 message_loop 的 RelayMessage 解析。
    let data: RelayMessage =
        serde_json::from_value(golden("relay_data_to_gateway")).expect("data 帧应可解析");
    match data {
        RelayMessage::Data { data, .. } => {
            let req: crate::acp::Request = serde_json::from_value(data)
                .expect("data 内层应是网关可处理的 JSON-RPC 请求");
            assert_eq!(req.method, "prompt");
        }
        other => panic!("应是 Data 帧，得到 {other:?}"),
    }
}

#[test]
fn relay_directed_response_carries_client_id() {
    // 网关→客户端定向响应：顶层 clientId + 标准 JSON-RPC 字段（relay 剥掉
    // clientId 后原样转发，所以其余字段必须是完整响应）。
    let v = golden("relay_directed_response");
    assert!(v.get("clientId").and_then(|c| c.as_str()).is_some());
    let resp: crate::acp::Response =
        serde_json::from_value(v).expect("剥 clientId 前后都应是合法响应");
    assert!(resp.result.is_some());
}

#[test]
fn external_prompt_params_parse() {
    let plain = golden("external_prompt_plain_request");
    let plain_req: crate::acp::Request =
        serde_json::from_value(plain.clone()).expect("普通轮请求应可解析");
    assert_eq!(plain_req.method, "prompt");
    let plain_params: crate::acp::PromptParams = serde_json::from_value(
        plain_req.params.expect("params 必填"),
    )
    .expect("PromptParams 应可解析");
    assert_eq!(plain_params.agent, "travel-planner");
    assert_eq!(plain_params.message, "只回：ok");
    assert!(plain_params.sessionTicket.is_none());
    assert!(plain_params.materials.is_none());
    assert!(plain_params.activeFlow.is_none());
    assert!(plain_params.borrower.is_none());

    // 借用轮五件套全部在场。
    let borrowed = golden("external_prompt_borrowed_request");
    let req: crate::acp::Request =
        serde_json::from_value(borrowed).expect("借道请求应可解析");
    let params: crate::acp::PromptParams =
        serde_json::from_value(req.params.expect("params 必填"))
            .expect("借用五件套字段名应与 PromptParams 一致");
    assert!(params.sessionTicket.is_some());
    assert!(params.materials.is_some());
    assert_eq!(params.activeFlow.as_deref(), Some("consultation"));
    assert_eq!(params.borrower.as_deref(), Some("friend-a"));
}

#[test]
fn external_initialize_result_key_set() {
    // handler.rs initialize 响应的键集契约（protocolVersion 整数 1 +
    // authenticated + serverInfo）。
    let v = golden("external_initialize_result");
    assert_eq!(v.get("protocolVersion"), Some(&serde_json::json!(1)));
    assert_eq!(
        v.get("authenticated").and_then(|a| a.as_bool()),
        Some(true)
    );
    let server = v.get("serverInfo").expect("serverInfo 必填");
    assert_eq!(server.get("name").and_then(|n| n.as_str()), Some("aginx"));
    assert!(server.get("version").is_some());
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    let mut expected = vec!["protocolVersion", "authenticated", "serverInfo"];
    expected.sort_unstable();
    assert_eq!(keys, expected);
}

#[test]
fn external_bind_device_params_camel() {
    // bindDevice 参数 camelCase（pairCode/deviceName）——BindParams 的
    // rename_all 契约。
    let v = golden("external_bind_device_request");
    let params = v.get("params").expect("params 必填");
    let mut keys: Vec<&str> = params.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["deviceName", "pairCode"]);
}

#[test]
fn external_stop_reason_vocabulary_is_end_turn_camel() {
    // 外部 stopReason 词汇 = endTurn（普通轮与借道轮一致）。借道轮的
    // end_turn 在网关翻译（adapter prompt_borrowed），此处锁死两种路径同词。
    let plain = golden("external_final_result_plain");
    assert_eq!(
        plain.pointer("/result/stopReason").and_then(|s| s.as_str()),
        Some("endTurn")
    );
    assert!(plain.get("id").is_none(), "最终响应不带 id（ack 吞铁则）");

    let borrowed = golden("external_final_result_borrowed");
    assert_eq!(
        borrowed.pointer("/result/stopReason").and_then(|s| s.as_str()),
        Some("endTurn")
    );
    assert!(borrowed.pointer("/result/sessionTicket").is_some());
    assert!(borrowed.pointer("/result/files").is_some());
}

#[test]
fn chunk_notification_shape() {
    let v = golden("external_chunk_notification");
    assert_eq!(v.get("method").and_then(|m| m.as_str()), Some("chunk"));
    assert_eq!(
        v.pointer("/params/text").and_then(|t| t.as_str()),
        Some("好的，我来规划")
    );
}

#[test]
fn external_final_result_translated_fields() {
    // §2.5 翻译轮最终结果：收割的真 sessionId + 可选成本三件套（camelCase），
    // 不带 id（ack 吞铁则同 plain/borrowed）。
    let v = golden("external_final_result_translated");
    let result = v.pointer("/result").expect("result 必填");
    assert_eq!(
        result.get("stopReason").and_then(|s| s.as_str()),
        Some("endTurn")
    );
    assert_eq!(
        result.get("sessionId").and_then(|s| s.as_str()),
        Some("b8f713a4-ea3b-4d6c-920b-87dc2a0403f0")
    );
    assert!(result.get("sessionId").unwrap().as_str().unwrap().chars().all(
        |c| c.is_ascii_alphanumeric() || c == '-' || c == '_'
    ));
    assert_eq!(result.get("costUsd").and_then(|c| c.as_f64()), Some(0.015));
    assert_eq!(result.get("durationMs").and_then(|d| d.as_u64()), Some(8400));
    assert_eq!(result.get("numTurns").and_then(|n| n.as_u64()), Some(1));
    assert!(v.get("id").is_none(), "最终响应不带 id（ack 吞铁则）");
}

#[test]
fn external_sessions_list_request_params() {
    // §2.4.1 sessions/list 请求参数形状：{agent}（台账按注册名查，无 cwd）。
    let v = golden("external_sessions_list_request");
    assert_eq!(
        v.get("method").and_then(|m| m.as_str()),
        Some("sessions/list")
    );
    let params = v.get("params").expect("params 必填");
    assert_eq!(params.get("agent").and_then(|a| a.as_str()), Some("claude"));
    assert!(params.get("cwd").is_none(), "台账语义下无 cwd 参数");
}

#[test]
fn external_sessions_list_result_deserializes() {
    // §2.4.1 结果形状：{sessions:[{sessionId,title,lastTs,turns}]}，
    // 用台账 SessionSummary 反序列化锁死字段名（事实源=网关台账，非 agent 私有存储）。
    let v = golden("external_sessions_list_result");
    let sessions = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .expect("sessions 数组必填");
    assert_eq!(sessions.len(), 1);
    let summary: crate::agent::ledger::SessionSummary =
        serde_json::from_value(sessions[0].clone()).expect("SessionSummary 字段名应与文档一致");
    assert_eq!(summary.turns, 2);
    assert!(summary.lastTs.ends_with('Z'));
}

#[test]
fn internal_initialize_uses_integer_version() {
    // 网关→桥 initialize 用整数 protocolVersion（adapter rpc! 的写法）。
    let v = golden("internal_initialize_request");
    assert_eq!(
        v.pointer("/params/protocolVersion"),
        Some(&serde_json::json!(1))
    );
}
