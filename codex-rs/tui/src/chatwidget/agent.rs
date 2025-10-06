use std::sync::Arc;

use codex_core::CodexConversation;
use codex_core::ConversationManager;
use codex_core::NewConversation;
use codex_core::config::Config;
use codex_core::protocol::Op;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

// 本文件负责在 TUI（终端 UI）端为与 Codex 后端的会话创建 "agent"。
// agent 的职责包括：
// - 在后台启动一个新的会话（或接管现有会话）；
// - 将会话产生的事件转发到应用层（通过 `AppEventSender`），以便 UI 渲染；
// - 接收来自 UI 的操作（`Op`）并转发到后端会话以提交处理结果。
//
// 文件提供两个公开函数：
// - `spawn_agent`：用于基于 `ConversationManager` 与配置创建并启动一个新会话的 agent。
// - `spawn_agent_from_existing`：用于为已存在的 `CodexConversation`（例如 fork 后的会话）启动 agent。
//
// 注：所有启动的任务都使用 `tokio::spawn` 异步运行，以免阻塞主线程。在运行时，这些
// agent 会在后台持续监听会话事件并转发到 UI。

/// Spawn the agent bootstrapper and op forwarding loop, returning the
/// `UnboundedSender<Op>` used by the UI to submit operations.
pub(crate) fn spawn_agent(
    config: Config,
    app_event_tx: AppEventSender,
    server: Arc<ConversationManager>,
) -> UnboundedSender<Op> {
    let (codex_op_tx, mut codex_op_rx) = unbounded_channel::<Op>();

    // `codex_op_tx` 是返回给调用者（通常是 UI 线程）的发送端，
    // UI 可以通过它向 agent 发送 `Op`（操作请求），由后台任务接收并提交到会话。
    // `codex_op_rx` 是接收端，由随后启动的任务监听。

    let app_event_tx_clone = app_event_tx.clone();
    tokio::spawn(async move {
        let NewConversation {
            conversation_id: _,
            conversation,
            session_configured,
        } = match server.new_conversation(config).await {
            Ok(v) => v,
            Err(e) => {
                // 初始化会话失败时记录错误（目前仅日志）。
                // TODO: 将此错误传递回 UI，让用户看到初始化失败的原因。
                tracing::error!("failed to initialize codex: {e}");
                return;
            }
        };

        // Forward the captured `SessionConfigured` event so it can be rendered in the UI.
        let ev = codex_core::protocol::Event {
            // The `id` does not matter for rendering, so we can use a fake value.
            id: "".to_string(),
            msg: codex_core::protocol::EventMsg::SessionConfigured(session_configured),
        };
        // 将会话已配置的事件发送到应用层，UI 可以据此显示会话相关的配置信息。
        app_event_tx_clone.send(AppEvent::CodexEvent(ev));

        let conversation_clone = conversation.clone();
        tokio::spawn(async move {
            // 该内部任务负责监听来自 UI（通过 `codex_op_tx`）的 `Op`，并将其提交到会话。
            // 这样做可以把提交操作放到单独的异步任务中，避免阻塞主事件循环。
            while let Some(op) = codex_op_rx.recv().await {
                let id = conversation_clone.submit(op).await;
                if let Err(e) = id {
                    // 提交失败时记录错误，但不做进一步处理（可根据需要改为向 UI 上报）。
                    tracing::error!("failed to submit op: {e}");
                }
            }
        });

        // 主循环：从会话中轮询事件（例如响应、状态更新等），并将事件转发到 UI。
        // `conversation.next_event().await` 会在会话有新事件时返回 `Ok(event)`，
        // 在会话结束或出错时返回 `Err`，从而结束循环并停止 agent。
        while let Ok(event) = conversation.next_event().await {
            app_event_tx_clone.send(AppEvent::CodexEvent(event));
        }
    });

    codex_op_tx
}

/// Spawn agent loops for an existing conversation (e.g., a forked conversation).
/// Sends the provided `SessionConfiguredEvent` immediately, then forwards subsequent
/// events and accepts Ops for submission.
pub(crate) fn spawn_agent_from_existing(
    conversation: std::sync::Arc<CodexConversation>,
    session_configured: codex_core::protocol::SessionConfiguredEvent,
    app_event_tx: AppEventSender,
) -> UnboundedSender<Op> {
    let (codex_op_tx, mut codex_op_rx) = unbounded_channel::<Op>();

    let app_event_tx_clone = app_event_tx.clone();
    tokio::spawn(async move {
        // Forward the captured `SessionConfigured` event so it can be rendered in the UI.
        let ev = codex_core::protocol::Event {
            id: "".to_string(),
            msg: codex_core::protocol::EventMsg::SessionConfigured(session_configured),
        };
        // 立即发送会话配置事件到 UI，使 UI 能够立刻渲染会话的配置信息（例如系统提示、参数等）。
        app_event_tx_clone.send(AppEvent::CodexEvent(ev));

        let conversation_clone = conversation.clone();
        tokio::spawn(async move {
            // 与 `spawn_agent` 中相同：监听来自 UI 的 `Op` 并提交到现有会话。
            while let Some(op) = codex_op_rx.recv().await {
                let id = conversation_clone.submit(op).await;
                if let Err(e) = id {
                    tracing::error!("failed to submit op: {e}");
                }
            }
        });

        // 持续从已存在的会话读取事件并转发给 UI。
        while let Ok(event) = conversation.next_event().await {
            app_event_tx_clone.send(AppEvent::CodexEvent(event));
        }
    });

    codex_op_tx
}
