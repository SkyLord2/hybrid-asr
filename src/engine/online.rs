use std::net::TcpStream;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error as WsError, Message, WebSocket, connect};

use crate::backend::RealtimeAsrBackend;
use crate::config::OnlineAsrConfig;
use crate::engine::online_auth::{get_app_access_token, refresh_app_access_token};
use crate::engine::online_protocol::{
    build_online_asr_request, inspect_online_asr_response, parse_online_asr_response,
};
use crate::error::HybridAsrResult;
use crate::types::AsrResult;

pub struct OnlineAsrBackend {
    options: OnlineAsrConfig,
    socket: Option<WebSocket<MaybeTlsStream<TcpStream>>>,
    pending_bytes: Vec<u8>,
    first_frame_sent: bool,
    last_progressive_text: Option<String>,
}

impl OnlineAsrBackend {
    pub fn new(options: OnlineAsrConfig) -> Self {
        Self {
            options,
            socket: None,
            pending_bytes: Vec::new(),
            first_frame_sent: false,
            last_progressive_text: None,
        }
    }

    fn connect_if_needed(&mut self) -> HybridAsrResult<()> {
        if self.socket.is_some() {
            return Ok(());
        }

        let app_access_token = get_app_access_token(&self.options)?;
        let connect_result = self.connect_with_token(&app_access_token);
        let (mut socket, _) = match connect_result {
            Ok(socket) => socket,
            Err(err) => {
                if !is_unauthorized_handshake_error(err.as_ref()) {
                    return Err(err);
                }
                eprintln!(
                    "在线 ASR 握手收到 401，强制刷新 App-Access-Token 后重试一次: url={}, authUrl={}",
                    self.options.online_url, self.options.app_auth_url
                );
                let refreshed_token = refresh_app_access_token(&self.options)?;
                self.connect_with_token(&refreshed_token)?
            }
        };
        configure_socket_timeout(
            socket.get_mut(),
            Duration::from_millis(20),
            Duration::from_secs(3),
        )?;
        self.socket = Some(socket);
        self.first_frame_sent = false;
        self.last_progressive_text = None;
        eprintln!(
            "在线 ASR WebSocket 已连接: url={}, authUrl={}, traceId={}, bizId={}",
            self.options.online_url,
            self.options.app_auth_url,
            self.options.trace_id,
            self.options.biz_id
        );
        Ok(())
    }

    fn connect_with_token(
        &self,
        app_access_token: &str,
    ) -> HybridAsrResult<(
        WebSocket<MaybeTlsStream<TcpStream>>,
        tungstenite::handshake::client::Response,
    )> {
        let mut request = self.options.online_url.clone().into_client_request()?;
        request
            .headers_mut()
            .insert("App-Access-Token", HeaderValue::from_str(app_access_token)?);
        request
            .headers_mut()
            .insert("K-Code", HeaderValue::from_str(&self.options.k_code)?);
        request.headers_mut().insert(
            "VISIT-K-Code",
            HeaderValue::from_str(&self.options.visit_k_code)?,
        );
        Ok(connect(request)?)
    }

    fn append_samples_as_bytes(&mut self, samples: &[f32]) {
        match self.options.audio_format.as_str() {
            "pcm_s16le" => {
                self.pending_bytes.extend(samples.iter().flat_map(|sample| {
                    let scaled = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
                    scaled.to_le_bytes()
                }));
            }
            _ => {
                self.pending_bytes
                    .extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
            }
        }
    }

    fn send_ready_frames(&mut self) -> HybridAsrResult<Vec<AsrResult>> {
        let mut results = Vec::new();
        // 在线 ASR 协议要求 status=2 的最后一帧仍然必须携带非空音频。
        // 因此流式发送阶段始终保留最后一个包，收尾时再把它作为 status=2 发出去。
        while self.pending_bytes.len() > self.options.frame_bytes {
            let frame: Vec<u8> = self
                .pending_bytes
                .drain(..self.options.frame_bytes)
                .collect();
            let status = if self.first_frame_sent { 1 } else { 0 };
            self.first_frame_sent = true;
            self.send_audio_frame(&frame, status)?;
            results.extend(self.read_available_results()?);
        }
        Ok(results)
    }

    fn send_audio_frame(&mut self, frame: &[u8], status: i32) -> HybridAsrResult<()> {
        self.connect_if_needed()?;
        let audio = STANDARD.encode(frame);
        // crate::report_info_log!(
        //     "在线 ASR 发送音频帧: status={}, audioBytes={}, audioBase64Len={}, traceId={}, bizId={}",
        //     status,
        //     frame.len(),
        //     audio.len(),
        //     self.options.trace_id,
        //     self.options.biz_id
        // );
        let request = build_online_asr_request(
            &audio,
            &self.options.trace_id,
            &self.options.biz_id,
            status,
            self.options.hot_words.as_deref(),
        );
        let message = serde_json::to_string(&request)?;
        let socket = self.socket.as_mut().ok_or("在线 ASR WebSocket 尚未连接")?;
        socket.send(Message::Text(message.into()))?;
        Ok(())
    }

    fn read_available_results(&mut self) -> HybridAsrResult<Vec<AsrResult>> {
        let mut results = Vec::new();
        for _ in 0..8 {
            let Some(socket) = self.socket.as_mut() else {
                break;
            };
            match socket.read() {
                Ok(Message::Text(text)) => {
                    log_incoming_response("text", &text);
                    if let Some(result) =
                        parse_online_asr_response(&text, self.options.sample_rate)?
                        && self.should_emit(&result)
                    {
                        results.push(result);
                    }
                }
                Ok(Message::Binary(bytes)) => {
                    let text = String::from_utf8(bytes.to_vec())?;
                    log_incoming_response("binary", &text);
                    if let Some(result) =
                        parse_online_asr_response(&text, self.options.sample_rate)?
                        && self.should_emit(&result)
                    {
                        results.push(result);
                    }
                }
                Ok(Message::Close(_)) => {
                    self.socket = None;
                    break;
                }
                Ok(Message::Ping(payload)) => {
                    if let Some(socket) = self.socket.as_mut() {
                        socket.send(Message::Pong(payload))?;
                    }
                }
                Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
                Err(WsError::Io(err))
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(WsError::AlreadyClosed) | Err(WsError::ConnectionClosed) => {
                    self.socket = None;
                    break;
                }
                Err(err) => return Err(err.into()),
            }
        }
        Ok(results)
    }

    fn should_emit(&mut self, result: &AsrResult) -> bool {
        // 按上层约定，在线 ASR 仅上报 sentence 最终结果；
        // progressive 中间结果只保留在调试日志里，不再向 Node 侧透出。
        if result.is_final {
            self.last_progressive_text = None;
            true
        } else {
            self.last_progressive_text = Some(result.text.clone());
            false
        }
    }

    fn finish_socket(&mut self) -> HybridAsrResult<Vec<AsrResult>> {
        let mut results = Vec::new();
        if !self.pending_bytes.is_empty() {
            let frame = std::mem::take(&mut self.pending_bytes);
            // 最后一个非空音频包必须带 status=2，不能再额外发送空结束帧。
            let status = 2;
            self.first_frame_sent = true;
            self.send_audio_frame(&frame, status)?;
            results.extend(self.read_available_results()?);
        }

        if self.first_frame_sent {
            for _ in 0..20 {
                let new_results = self.read_available_results()?;
                let has_final = new_results.iter().any(|result| result.is_final);
                results.extend(new_results);
                if has_final {
                    break;
                }
            }
        }

        if let Some(mut socket) = self.socket.take() {
            let _ = socket.close(None);
        }
        self.first_frame_sent = false;
        self.last_progressive_text = None;
        Ok(results)
    }
}

fn log_incoming_response(message_type: &str, text: &str) {
    match inspect_online_asr_response(text) {
        Ok(info) => {
            eprintln!(
                "在线 ASR 收到响应: transport={}, code={}, headerStatus={}, msgtype={}",
                message_type,
                normalize_debug_field(&info.code),
                normalize_debug_field(&info.header_status),
                normalize_debug_field(&info.msg_type)
            );
        }
        Err(err) => {
            eprintln!(
                "在线 ASR 收到响应: transport={}, debugParseError={}, rawLen={}",
                message_type,
                err,
                text.len()
            );
        }
    }
}

fn normalize_debug_field(value: &str) -> &str {
    if value.is_empty() { "<empty>" } else { value }
}

fn is_unauthorized_handshake_error(err: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    err.downcast_ref::<WsError>()
        .map(|ws_err| match ws_err {
            WsError::Http(response) => response.status().as_u16() == 401,
            WsError::HttpFormat(err) => err.to_string().contains("401"),
            _ => false,
        })
        .unwrap_or(false)
}

fn configure_socket_timeout(
    stream: &mut MaybeTlsStream<TcpStream>,
    read_timeout: Duration,
    write_timeout: Duration,
) -> HybridAsrResult<()> {
    match stream {
        MaybeTlsStream::Plain(stream) => {
            stream.set_read_timeout(Some(read_timeout))?;
            stream.set_write_timeout(Some(write_timeout))?;
        }
        MaybeTlsStream::NativeTls(stream) => {
            stream.get_ref().set_read_timeout(Some(read_timeout))?;
            stream.get_ref().set_write_timeout(Some(write_timeout))?;
        }
        _ => {}
    }
    Ok(())
}

impl RealtimeAsrBackend for OnlineAsrBackend {
    fn start(&mut self) -> HybridAsrResult<()> {
        self.connect_if_needed()
    }

    fn accept_samples(&mut self, samples: &[f32]) -> HybridAsrResult<Vec<AsrResult>> {
        if samples.is_empty() {
            return self.read_available_results();
        }
        self.append_samples_as_bytes(samples);
        self.send_ready_frames()
    }

    fn reset(&mut self) -> HybridAsrResult<()> {
        if self.socket.is_some() {
            let _ = self.finish_socket();
        }
        self.pending_bytes.clear();
        self.first_frame_sent = false;
        self.last_progressive_text = None;
        self.connect_if_needed()
    }

    fn prepare_next_turn(&mut self) -> HybridAsrResult<()> {
        // Reuse the online backend instance across turns. Internally this may
        // flush and reconnect, but the caller keeps the same session object.
        self.reset()
    }

    fn finish(&mut self) -> HybridAsrResult<Vec<AsrResult>> {
        if self.socket.is_none() && self.pending_bytes.is_empty() {
            return Ok(Vec::new());
        }
        self.finish_socket()
    }
}
