use serde::Serialize;
use serde_json::Value;

use crate::types::AsrResult;

#[derive(Debug, Clone)]
pub struct OnlineAsrResponseDebugInfo {
    pub code: String,
    pub header_status: String,
    pub msg_type: String,
}

#[derive(Serialize)]
pub struct OnlineAsrRequest<'a> {
    #[serde(rename = "type")]
    pub request_type: &'static str,
    pub audio: &'a str,
    pub config: OnlineAsrRequestConfig<'a>,
}

#[derive(Serialize)]
pub struct OnlineAsrRequestConfig<'a> {
    #[serde(rename = "traceId")]
    pub trace_id: &'a str,
    #[serde(rename = "bizId")]
    pub biz_id: &'a str,
    pub status: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<&'a str>,
}

pub fn build_online_asr_request<'a>(
    audio: &'a str,
    trace_id: &'a str,
    biz_id: &'a str,
    status: i32,
    hot_words: Option<&'a str>,
) -> OnlineAsrRequest<'a> {
    OnlineAsrRequest {
        request_type: "audio",
        audio,
        config: OnlineAsrRequestConfig {
            trace_id,
            biz_id,
            status,
            text: hot_words,
        },
    }
}

pub fn parse_online_asr_response(
    text: &str,
    sample_rate: i32,
) -> Result<Option<AsrResult>, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|err| format!("在线 ASR 响应 JSON 解析失败: {}", err))?;
    ensure_success_code(&value)?;

    let result = match value.pointer("/data/payload/result") {
        Some(result) => result,
        None => return Ok(None),
    };
    let transcribed_text = extract_words(result);
    if transcribed_text.trim().is_empty() {
        return Ok(None);
    }

    let msg_type = result
        .get("msgtype")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let is_final = msg_type == "sentence";
    let start_time = result.get("bg").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
    let raw_end_time = result.get("ed").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
    let end_time = if raw_end_time > start_time {
        raw_end_time
    } else {
        start_time
    };
    let start_sample = (start_time * sample_rate as f64).round() as i64;
    let sample_count = ((end_time - start_time).max(0.0) * sample_rate as f64).round() as i64;

    Ok(Some(AsrResult {
        text: transcribed_text,
        start_time,
        end_time,
        start_sample,
        sample_count,
        is_final,
    }))
}

pub fn inspect_online_asr_response(text: &str) -> Result<OnlineAsrResponseDebugInfo, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|err| format!("在线 ASR 响应 JSON 解析失败: {}", err))?;
    Ok(OnlineAsrResponseDebugInfo {
        code: format_json_value(value.get("code")),
        header_status: format_json_value(value.pointer("/data/header/status")),
        msg_type: value
            .pointer("/data/payload/result/msgtype")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

fn ensure_success_code(value: &Value) -> Result<(), String> {
    if let Some(code) = value.get("code")
        && !is_success_code(code)
    {
        return Err(format!(
            "在线 ASR 服务返回失败 code={}, message={}",
            code,
            value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ));
    }

    if let Some(header_code) = value.pointer("/data/header/code")
        && !is_success_code(header_code)
    {
        return Err(format!(
            "在线 ASR header 返回失败 code={}, message={}",
            header_code,
            value
                .pointer("/data/header/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ));
    }

    Ok(())
}

fn is_success_code(value: &Value) -> bool {
    value.as_i64() == Some(0) || value.as_str() == Some("0")
}

fn extract_words(result: &Value) -> String {
    result
        .get("ws")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .flat_map(|item| {
                    item.get("cw")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .filter_map(|cw| cw.get("w").and_then(Value::as_str))
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn format_json_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{inspect_online_asr_response, parse_online_asr_response};

    #[test]
    fn extracts_progressive_text_from_online_response() {
        let payload = r#"{
            "code":"0",
            "data":{
                "payload":{
                    "result":{
                        "bg":1000,
                        "ed":1500,
                        "msgtype":"progressive",
                        "ws":[
                            {"cw":[{"w":"你好"}]},
                            {"cw":[{"w":"世界"}]}
                        ]
                    }
                },
                "header":{"code":0,"message":"success"}
            }
        }"#;
        let result = parse_online_asr_response(payload, 16000).unwrap().unwrap();
        assert_eq!(result.text, "你好世界");
        assert!(!result.is_final);
        assert_eq!(result.start_sample, 16000);
    }

    #[test]
    fn extracts_response_debug_info() {
        let payload = r#"{
            "code":"0",
            "data":{
                "payload":{
                    "result":{
                        "msgtype":"sentence"
                    }
                },
                "header":{"status":2}
            }
        }"#;
        let info = inspect_online_asr_response(payload).unwrap();
        assert_eq!(info.code, "0");
        assert_eq!(info.header_status, "2");
        assert_eq!(info.msg_type, "sentence");
    }
}
