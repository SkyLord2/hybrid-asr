# hybrid-asr

`hybrid-asr` 是一个统一的实时语音识别 Rust SDK，对上提供稳定的 `HybridAsrSession` 会话接口，对下按配置切换本地 SenseVoice 后端或在线流式后端。

当前代码的几个关键点已经固定下来：

- 本地模式统一收口到 `ASR 模型目录 + VAD 模型目录` 的聚合资源准备流程
- 新增 `inspect_local_asr_assets_download()`，方便上层在真正建会话前判断“是否会触发下载”
- 新增 `prepare_local_asr_assets_with_event_hub()`，用于统一发出 `missing / downloading / downloaded / failed` 模型事件
- 新增 `HybridAsrSession::new_with_event_hub()`，解决“本地模型早期事件发生在订阅前”的时序问题

## 1. 当前能力

- 统一配置入口：`HybridAsrConfig`
- 本地模式：SenseVoice + `vad-detector`
- 在线模式：WebSocket 流式识别 + App Token 鉴权
- 统一会话模型：`HybridAsrSession`
- 统一结果结构：`AsrResult`
- 多轮热复用：`finish_turn()` + `prepare_next_turn()`
- 本地资源预检：`prepare_local_asr_assets()`
- 本地资源显式下载：`download_local_asr_assets()`
- 模型下载预判：`inspect_local_asr_assets_download()`
- 统一事件总线：`AsrEventHub` / `AsrEvent`

补充说明：
- `AsrEventHub` 现在支持显式 `close()` 生命周期语义。
- `close()` 后现有订阅端会自然收到 channel 断开，可据此安全退出桥接循环。
- `close()` 后再次 `subscribe()` 会返回已断开的 `Receiver`，事件流不会被重新打开。

## 2. 安装

```toml
[dependencies]
hybrid-asr = { path = "crates/hybrid-asr" }
```

## 3. 核心类型

### 3.1 `HybridAsrConfig`

```rust
pub enum HybridAsrConfig {
    Local(LocalAsrConfig),
    Online(OnlineAsrConfig),
}
```

### 3.2 `LocalAsrConfig`

| 字段 | 说明 |
| --- | --- |
| `asr_model_dir` | ASR 模型根目录，目录内固定包含 `model.int8.onnx` 与 `tokens.txt` |
| `vad_model_dir` | VAD 模型根目录，目录内固定包含 `silero_vad_v5.onnx` |
| `auto_download` | 缺模时是否允许自动下载，默认 `false` |
| `download_url` | SenseVoice 主模型 bundle 压缩包的完整下载 URL |
| `target_dir` | ASR 主模型安装目标根目录 |
| `vad_download_url` | VAD 模型 bundle 压缩包的完整下载 URL |
| `vad_target_dir` | VAD 模型安装目标根目录 |
| `sample_rate` | 采样率，当前本地模式按 `16000` 使用 |
| `num_threads` | SenseVoice 推理线程数 |
| `use_itn` | 是否启用 ITN |
| `provider` | 推理 provider，例如 `cpu` |
| `min_silence_duration` | VAD 最短静音时长 |
| `min_speech_duration` | VAD 最短语音时长 |
| `vad_threshold` | VAD 阈值 |

补充说明：

- `download_url` / `target_dir` 负责主 ASR 模型目录，目录内固定解析 `model.int8.onnx` 与 `tokens.txt`
- `vad_download_*` / `vad_target_dir` 负责 VAD 模型
- 如果 `vad_download_*` 未提供，会自动回退到通用下载字段，兼容旧调用方
- 如果 `target_dir` 或 `vad_target_dir` 为空，会回退到默认安装根目录 `~/.ai-client/models`
- 目录级固定布局约定如下：
  - `asr_model_dir/model.int8.onnx`
  - `asr_model_dir/tokens.txt`
  - `vad_model_dir/silero_vad_v5.onnx`

### 3.3 `OnlineAsrConfig`

| 字段 | 说明 |
| --- | --- |
| `online_url` | 在线 ASR WebSocket 地址 |
| `app_auth_url` | App Token 鉴权地址 |
| `k_code` | 调用方 K-Code |
| `k_secret` | 调用方 K-Secret |
| `visit_k_code` | 被调方 Visit-K-Code |
| `trace_id` | 链路追踪 ID |
| `biz_id` | 业务透传 ID |
| `hot_words` | 热词字符串，可选 |
| `sample_rate` | 采样率 |
| `audio_format` | 在线音频格式，例如 `pcm_s16le` |
| `frame_bytes` | 每帧字节数 |

### 3.4 常量与辅助函数

`config.rs` 当前导出了几组上层常用默认值：

- `DEFAULT_ONLINE_ASR_URL`
- `DEFAULT_ONLINE_AUDIO_FORMAT`
- `DEFAULT_ONLINE_FRAME_BYTES`
- `DEFAULT_ONLINE_VISIT_K_CODE`
- `MAX_ONLINE_FRAME_BYTES`
- `infer_app_auth_url_from_online_url()`

## 4. 本地资源预检与下载策略

### 4.1 当前真实触发时机

这是这次 README 最需要对齐代码的一点：

- `HybridAsrSession::new()` 在 `Local` 模式下会立即构造本地 backend
- backend 构造阶段会先执行 `prepare_local_asr_assets_with_event_hub()`
- 也就是说，本地 ASR 的模型预检、受控自动下载、目录解析发生在 `new()` / `new_with_event_hub()` 阶段，而不是 `start()` 阶段

因此：

- 如果本地模型已经就绪，`new()` 会较快返回
- 如果本地模型缺失、`auto_download = true` 且下载配置完整，crate 会在构造阶段同步下载并安装，再返回 `HybridAsrSession`
- 如果本地模型缺失且 `auto_download = false`，会立即返回错误，提示先调用显式下载 API
- 如果本地模型缺失但下载配置不完整，会直接返回错误

### 4.2 公开预检 API

#### `inspect_local_asr_assets_download()`

```rust
pub fn inspect_local_asr_assets_download(config: &LocalAsrConfig) -> HybridAsrResult<bool>
```

返回值语义固定为：

- `Ok(false)`：主 ASR 与 VAD 都已可用，不需要下载
- `Ok(true)`：至少一类资源缺失，但下载参数齐全，可以安全触发显式下载
- `Err(...)`：存在缺失资源，但下载参数不合法，当前不能伪装成“已开始后台下载”

这个 API 的定位不是替代 `prepare_local_asr_assets()`，而是让上层包装层有机会实现：

- 首次缺模时立即返回
- 由上层改走显式下载 API
- 下载完成后重新发起会话创建

注意：`hybrid-asr` crate 自身仍然是同步资源准备；显式下载和是否允许自动下载由调用方通过 `auto_download` 与显式下载 API 控制。

#### `prepare_local_asr_assets()`

```rust
pub fn prepare_local_asr_assets(
    config: &LocalAsrConfig,
    observer: Option<ModelDownloadObserver>,
) -> HybridAsrResult<PreparedLocalAsrAssets>
```

这个 API 会聚合处理：

- ASR 模型目录中的 `model.int8.onnx`
- ASR 模型目录中的 `tokens.txt`
- VAD 模型

返回结构：

- `resolved_config`：回填真实目录后的配置
- `downloaded_anything`：本次是否发生过真实下载
- `install_root`：主 ASR 模型的安装根目录

`install_root` 仍然保持“主 ASR 安装根”的语义，不额外返回 VAD 安装根。

补充说明：

- `prepare_local_asr_assets()` 受 `auto_download` 控制
- `download_local_asr_assets()` 不受 `auto_download` 控制，语义固定为“显式触发主 ASR + VAD 下载”

#### `prepare_local_asr_assets_with_event_hub()`

```rust
pub fn prepare_local_asr_assets_with_event_hub(
    config: &LocalAsrConfig,
    observer: Option<ModelDownloadObserver>,
    event_hub: Option<&AsrEventHub>,
) -> HybridAsrResult<PreparedLocalAsrAssets>
```

这个 API 是当前最完整的“预检 + 事件”入口：

- 进入下载前主动发 `missing`
- 下载进度通过 `observer` 映射成 `downloading`
- 下载成功后补发 `downloaded`
- 下载或安装失败时补发 `failed`

资源类型固定为：

- `resource_kind = asr`
- `resource_kind = vad`

阶段固定为：

- `missing`
- `downloading`
- `downloaded`
- `failed`

#### `download_local_asr_assets_with_event_hub()`

```rust
pub fn download_local_asr_assets_with_event_hub(
    config: &LocalAsrConfig,
    observer: Option<ModelDownloadObserver>,
    event_hub: Option<&AsrEventHub>,
) -> HybridAsrResult<PreparedLocalAsrAssets>
```

这个 API 与 `prepare_local_asr_assets_with_event_hub()` 共享同一套事件语义，但会忽略 `auto_download` 开关，适合“显式点击下载”或顶层统一下载入口。

## 5. 统一事件总线

### 5.1 `AsrEvent`

```rust
pub enum AsrEvent {
    StateChanged(AsrStateTransitionEvent),
    Session(SessionEvent),
    Model(ModelLifecycleEvent),
    Error(String),
}
```

说明：

- `hybrid-asr` 当前不会把识别结果做成事件流
- 识别结果仍然通过 `accept_samples()` / `finish()` 的返回值获取
- 模型事件只在 `Local` 模式下产生，在线模式不会伪造 `Model(...)`

### 5.2 为什么新增 `new_with_event_hub()`

旧的用法通常是：

1. `HybridAsrSession::new(config)`
2. `session.subscribe_events()`

但本地模式下，模型事件可能在 `new()` 的 backend 构造期间就已经发出。  
如果你在 `new()` 返回之后才订阅，就可能丢掉最早的 `missing / downloading` 事件。

现在推荐两种用法：

- 普通业务：直接 `new()`，适合你只关心创建完成后的状态/错误/会话事件
- 早期模型事件业务：先建 `AsrEventHub` 并订阅，再调用 `new_with_event_hub()`

### 5.3 订阅构造早期模型事件

```rust
use hybrid_asr::{
    AsrEvent, AsrEventHub, HybridAsrConfig, HybridAsrSession, LocalAsrConfig,
    ModelLifecyclePhase,
};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let event_hub = Arc::new(AsrEventHub::default());
    let events = event_hub.subscribe();

    let _session = HybridAsrSession::new_with_event_hub(
        HybridAsrConfig::Local(LocalAsrConfig {
            asr_model_dir: "./sense-voice-zh-en".to_string(),
            vad_model_dir: "./silero_vad".to_string(),
            auto_download: true,
            download_url: Some("http://123.56.87.24/models/sense-voice-zh-en.bundle.zip".to_string()),
            target_dir: None,
            vad_download_url: Some("http://123.56.87.24/models/silero_vad.bundle.zip".to_string()),
            vad_target_dir: None,
            sample_rate: 16_000,
            num_threads: 2,
            use_itn: true,
            provider: "cpu".to_string(),
            min_silence_duration: 0.5,
            min_speech_duration: 0.25,
            vad_threshold: 0.5,
        }),
        Arc::clone(&event_hub),
    )?;

    while let Ok(event) = events.try_recv() {
        if let AsrEvent::Model(model) = event {
            match model.phase {
                ModelLifecyclePhase::Missing => {
                    println!("{} 缺失: {}", model.resource_kind.as_str(), model.message);
                }
                ModelLifecyclePhase::Downloading => {
                    println!(
                        "{} 下载中: {:?}/{:?}",
                        model.resource_kind.as_str(),
                        model.downloaded_bytes,
                        model.total_bytes
                    );
                }
                ModelLifecyclePhase::Downloaded => {
                    println!("{} 已就绪: {}", model.resource_kind.as_str(), model.target_dir.display());
                }
                ModelLifecyclePhase::Failed => {
                    println!("{} 失败: {}", model.resource_kind.as_str(), model.message);
                }
            }
        }
    }

    Ok(())
}
```

## 6. 会话生命周期

`HybridAsrSession` 是当前 crate 对外主入口：

```rust
pub struct HybridAsrSession {
    backend: Box<dyn RealtimeAsrBackend>,
    state: AsrState,
}
```

### 6.1 常用方法

- `new(config)`：构造真实 backend
- `new_with_event_hub(config, event_hub)`：允许先订阅再构造
- `new_with_backend(backend)`：测试或自定义 backend 注入
- `subscribe_events()`：订阅统一事件流
- `start()`：启动 backend，成功后进入 `Ready`
- `accept_samples(samples)`：喂入 `&[f32]` PCM 样本
- `pause()`：暂停当前会话
- `resume()`：恢复当前会话
- `finish()`：结束整个 ASR 会话
- `finish_turn()`：结束当前轮识别
- `prepare_next_turn()`：为下一轮清理缓存并回到 `Ready`
- `reset()`：重置 backend 并回到 `Ready`
- `state()`：读取当前状态

### 6.2 状态说明

当前公开状态：

- `Idle`
- `Initializing`
- `Ready`
- `Recognizing`
- `Paused`
- `Finishing`
- `Finished`
- `Error`

常见流转：

- `new()` 后初始为 `Idle`
- `start()` 成功后：`Idle -> Initializing -> Ready`
- `accept_samples()` 时进入 `Recognizing`
- `pause()` 后进入 `Paused`
- `finish()` 后进入 `Finishing -> Finished`
- `prepare_next_turn()` 或 `reset()` 会回到 `Ready`

### 6.3 `finish()` 与 `finish_turn()`

语义上：

- `finish()`：结束整个 ASR 会话
- `finish_turn()`：结束当前一轮，为下一轮热复用保留语义

当前实现上，`finish_turn()` 直接委托给 `finish()`；  
后续如需区分更多会话收尾策略，仍然建议业务方优先使用 `finish_turn()` 表达“多轮对话的一轮结束”。

## 7. 输入音频格式

`accept_samples()` 当前接收：

- 单声道
- `f32`
- PCM 样本切片 `&[f32]`

调用方需要自己保证采样率与配置一致。

## 8. 使用示例

### 8.1 本地模式最小示例

```rust
use hybrid_asr::{AsrState, HybridAsrConfig, HybridAsrSession, LocalAsrConfig};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = HybridAsrConfig::Local(LocalAsrConfig {
        asr_model_dir: "./sense-voice-zh-en".to_string(),
        vad_model_dir: "./silero_vad".to_string(),
        auto_download: true,
        download_url: Some("http://123.56.87.24/models/sense-voice-zh-en.bundle.zip".to_string()),
        target_dir: None,
        vad_download_url: Some("http://123.56.87.24/models/silero_vad.bundle.zip".to_string()),
        vad_target_dir: None,
        sample_rate: 16_000,
        num_threads: 2,
        use_itn: true,
        provider: "cpu".to_string(),
        min_silence_duration: 0.5,
        min_speech_duration: 0.25,
        vad_threshold: 0.5,
    });

    // 注意：Local 模式下，这一步只会在 auto_download=true 时触发同步下载。
    let mut session = HybridAsrSession::new(config)?;

    session.start()?;
    assert_eq!(session.state(), AsrState::Ready);

    // 这里示意喂入一小段静音样本；真实业务请传入麦克风或文件 PCM。
    let results = session.accept_samples(&vec![0.0; 1600])?;
    println!("results={results:?}");

    let final_results = session.finish()?;
    println!("final_results={final_results:?}");
    Ok(())
}
```

### 8.2 只做预检，不立即建会话

```rust
use hybrid_asr::{LocalAsrConfig, inspect_local_asr_assets_download};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = LocalAsrConfig {
        asr_model_dir: "./sense-voice-zh-en".to_string(),
        vad_model_dir: "./silero_vad".to_string(),
        auto_download: false,
        download_url: Some("http://123.56.87.24/models/sense-voice-zh-en.bundle.zip".to_string()),
        target_dir: None,
        vad_download_url: Some("http://123.56.87.24/models/silero_vad.bundle.zip".to_string()),
        vad_target_dir: None,
        sample_rate: 16_000,
        num_threads: 2,
        use_itn: true,
        provider: "cpu".to_string(),
        min_silence_duration: 0.5,
        min_speech_duration: 0.25,
        vad_threshold: 0.5,
    };

    match inspect_local_asr_assets_download(&config)? {
        false => println!("本地 ASR 与 VAD 都已可用，可直接建会话"),
        true => println!("至少一类资源缺失，但下载配置合法，可改走显式下载 API"),
    }

    Ok(())
}
```

### 8.3 多轮热复用示例

```rust
use hybrid_asr::{HybridAsrConfig, HybridAsrSession, LocalAsrConfig};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = HybridAsrConfig::Local(LocalAsrConfig {
        asr_model_dir: "./sense-voice-zh-en".to_string(),
        vad_model_dir: "./silero_vad".to_string(),
        auto_download: true,
        download_url: Some("http://123.56.87.24/models/sense-voice-zh-en.bundle.zip".to_string()),
        target_dir: None,
        vad_download_url: Some("http://123.56.87.24/models/silero_vad.bundle.zip".to_string()),
        vad_target_dir: None,
        sample_rate: 16_000,
        num_threads: 2,
        use_itn: true,
        provider: "cpu".to_string(),
        min_silence_duration: 0.5,
        min_speech_duration: 0.25,
        vad_threshold: 0.5,
    });

    let mut session = HybridAsrSession::new(config)?;
    session.start()?;

    for turn in 1..=3 {
        let _ = session.accept_samples(&vec![0.0; 1600])?;
        let final_results = session.finish_turn()?;
        println!("turn={turn}, final_results={final_results:?}");

        if turn < 3 {
            session.prepare_next_turn()?;
        }
    }

    Ok(())
}
```

## 9. 与 `vad-detector` / `model-installer` 的关系

- `vad-detector`：负责端点检测与 VAD 模型准备
- `hybrid-asr`：负责聚合 ASR 主模型与 VAD 资源、构造识别器、维护会话语义
- `model-installer`：负责下载清单解析、压缩包下载、校验和解压安装

职责边界是：

- `vad-detector` 专注“切分语音片段”
- `hybrid-asr` 专注“把片段识别成文本，并提供统一会话与事件语义”
- `model-installer` 专注“模型文件下载与安装”

## 10. 测试与验证

```bash
cargo check --manifest-path crates/hybrid-asr/Cargo.toml
cargo test --manifest-path crates/hybrid-asr/Cargo.toml
```
