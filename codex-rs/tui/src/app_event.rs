use codex_core::protocol::ConversationHistoryResponseEvent;
use codex_core::protocol::Event;
use codex_file_search::FileMatch;
use ratatui::text::Line;

use crate::history_cell::HistoryCell;

use codex_core::protocol::AskForApproval;
use codex_core::protocol::SandboxPolicy;
use codex_core::protocol_config_types::ReasoningEffort;

// `AppEvent` 是在 TUI 应用内部用于不同组件之间传递的事件总线载体。
//
// 说明：UI 层（widgets）不会直接依赖底层会话或 agent 的通道接口，
// 相反它们通过发送 `AppEvent` 到应用层的分发器，应用层再根据类型
// 将事件转发给相应的处理器（例如 agent、文件搜索模块或历史渲染器）。
//
// 该枚举包含了来自后端（`CodexEvent`）、用户动作（`NewSession`、`CodexOp` 等）、
// 异步子系统结果（`FileSearchResult`、`DiffResult`）以及 UI 状态更新（如动画控制、
// 模型/策略更新）等多类事件。
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum AppEvent {
    /// 从 codex 后端转发来的原生 `Event`（包含会话输出、配置事件等）。
    /// 这些事件通常来源于 `agent`（见 `chatwidget/agent.rs`），并最终由 UI 渲染。
    CodexEvent(Event),

    /// 请求开始一个新的会话（例如通过 UI 的 New Session 操作触发）。
    NewSession,

    /// 请求优雅退出应用（UI 命令或快捷键触发）。应用层应对此事件进行清理并退出。
    ExitRequest,

    /// 将一个 `Op` 转发给 agent 提交到后端会话。
    /// 使用 `AppEvent` 的目的是避免在 UI 各层之间直接穿透传递通道引用。
    CodexOp(codex_core::protocol::Op),

    /// 发起一次异步文件搜索，`String` 为搜索查询（通常是 `@` 后的文本）。
    /// 应用层负责管理并可能在新的搜索到来时取消先前的进行中搜索。
    StartFileSearch(String),

    /// 异步文件搜索的结果。包含原始 `query`（用于判断结果是否仍然相关）
    /// 和匹配结果列表 `matches`（`codex_file_search::FileMatch`）。
    FileSearchResult {
        query: String,
        matches: Vec<FileMatch>,
    },

    /// `/diff` 命令的计算结果，`String` 为 diff 文本。UI 可将其展示在相应面板。
    DiffResult(String),

    /// 将一组历史文本行插入到历史视图（用于渲染历史输出片段）。
    InsertHistoryLines(Vec<Line<'static>>),

    /// 将自定义的 `HistoryCell` 插入历史视图，该 trait 用于延迟渲染或更复杂的历史单元。
    InsertHistoryCell(Box<dyn HistoryCell>),

    /// 启动历史提交（或等待）动画的命令事件（用于 UI 动画控制）。
    StartCommitAnimation,
    /// 停止提交动画。
    StopCommitAnimation,
    /// 提交动画的周期性 tick，用于驱动帧更新。
    CommitTick,

    /// 更新当前的推理强度（ReasoningEffort），供正在运行的 widget/会话使用。
    UpdateReasoningEffort(ReasoningEffort),

    /// 更新当前使用的模型标识（model slug），UI 与会话可以据此变更推理行为或显示。
    UpdateModel(String),

    /// 更新审批策略（AskForApproval），用于控制在需要人工确认时的行为。
    UpdateAskForApprovalPolicy(AskForApproval),

    /// 更新沙箱策略（SandboxPolicy），影响执行/限制相关的行为。
    UpdateSandboxPolicy(SandboxPolicy),

    /// 来自后端会话的会话历史快照事件，包含会话历史的响应数据，
    /// UI 可用它来重播或渲染完整会话历史。
    ConversationHistory(ConversationHistoryResponseEvent),
}
