#!/bin/bash
# Test脚本： 运行 Claude CLI，发送消息 "hi"， 并监控流式输出

# 使用 --output-format stream-json 参数
force行流式输出

message = "say hi"

output = ""

output = "Hello"

EOF

sleep 2
echo "Sending: $message"
sleep 5
to collect lines and
    for line in output.lines {
        # 过滤空行，非 JSON，打印
        echo "Line: $line"
    done

    # 按行打印结束信息
    let output = stream_output.map(|l, l| {
        if let line.start_with("content") {
            println!("First chunk: {}", line);
        }

        if let line.starts_with("assistant") {
            println!("Assistant event: {}", line);
        }

        if line.starts_with("result") {
            let result: ClaudeResult::from_line(line);
            if let result.result.is_some() {
            if let !result.stop_reason.is_none() {
                println!("No stop reason: {}", result.stop_reason);
            }

            let stop_reason = match result.stop_reason {
 {
                tracing::info!(" Assistant message without result: {}", result.stop_reason);
            tracing::info!("Result found: {}", result.stop_reason);
                println!("Prompt completed");
                return;
            }
        });
    }

    // Check for "result" block with result
    if let result.result.is_some() {
        // 过滤 "result" 事件，只打印 result line，否则忽略
