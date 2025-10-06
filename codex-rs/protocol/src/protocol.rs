//! 定义客户端和代理之间 Codex 会话的协议。
//!
//! 使用 SQ (提交队列) / EQ (事件队列) 模式在用户和代理之间进行异步通信。
//!
//! Defines the protocol for a Codex session between a client and an agent.
//!
//! Uses a SQ (Submission Queue) / EQ (Event Queue) pattern to asynchronously communicate
//! between user and agent.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use crate::custom_prompts::CustomPrompt;
use mcp_types::CallToolResult;
use mcp_types::Tool as McpTool;
use serde::Deserialize;
use serde::Serialize;
use serde_bytes::ByteBuf;
use strum_macros::Display;
use ts_rs::TS;
use uuid::Uuid;

use crate::config_types::ReasoningEffort as ReasoningEffortConfig;
use crate::config_types::ReasoningSummary as ReasoningSummaryConfig;
use crate::message_history::HistoryEntry;
use crate::models::ResponseItem;
use crate::parse_command::ParsedCommand;
use crate::plan_tool::UpdatePlanArgs;

/// 提交队列条目 - 来自用户的请求
/// Submission Queue Entry - requests from user
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Submission {
    /// 此提交的唯一 ID，用于与事件关联
    /// Unique id for this Submission to correlate with Events
    pub id: String,
    /// 有效载荷
    /// Payload
    pub op: Op,
}

/// 提交操作
/// Submission operation
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
#[non_exhaustive]
pub enum Op {
    /// 终止当前任务
    /// 服务器响应发送 [`EventMsg::TurnAborted`]
    /// Abort current task.
    /// This server sends [`EventMsg::TurnAborted`] in response.
    Interrupt,

    /// 来自用户的输入
    /// Input from the user
    UserInput {
        /// 用户输入项，参见 `InputItem`
        /// User input items, see `InputItem`
        items: Vec<InputItem>,
    },

    /// 类似于 [`Op::UserInput`]，但包含 [`crate::codex_conversation::CodexConversation`] 一轮对话所需的额外上下文
    /// Similar to [`Op::UserInput`], but contains additional context required
    /// for a turn of a [`crate::codex_conversation::CodexConversation`].
    UserTurn {
        /// 用户输入项，参见 `InputItem`
        /// User input items, see `InputItem`
        items: Vec<InputItem>,

        /// 与 [`SandboxPolicy`] 和可能的工具调用（如 `local_shell`）一起使用的 `cwd`
        /// `cwd` to use with the [`SandboxPolicy`] and potentially tool calls
        /// such as `local_shell`.
        cwd: PathBuf,

        /// 用于命令批准的策略
        /// Policy to use for command approval.
        approval_policy: AskForApproval,

        /// 用于工具调用（如 `local_shell`）的策略
        /// Policy to use for tool calls such as `local_shell`.
        sandbox_policy: SandboxPolicy,

        /// 必须是与此对话关联的 [`crate::client::ModelClient`] 的有效模型标识符
        /// Must be a valid model slug for the [`crate::client::ModelClient`]
        /// associated with this conversation.
        model: String,

        /// 仅当模型配置为使用推理时才会生效
        /// Will only be honored if the model is configured to use reasoning.
        effort: ReasoningEffortConfig,

        /// 仅当模型配置为使用推理时才会生效
        /// Will only be honored if the model is configured to use reasoning.
        summary: ReasoningSummaryConfig,
    },

    /// 覆盖后续轮次的持久性轮次上下文的部分内容
    ///
    /// 所有字段都是可选的；省略时，保留现有值
    /// 这不会排队任何输入 - 它只更新用于将来 `UserInput` 轮次的默认值
    /// Override parts of the persistent turn context for subsequent turns.
    ///
    /// All fields are optional; when omitted, the existing value is preserved.
    /// This does not enqueue any input – it only updates defaults used for
    /// future `UserInput` turns.
    OverrideTurnContext {
        /// 用于沙盒/工具调用的更新 `cwd`
        /// Updated `cwd` for sandbox/tool calls.
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,

        /// 更新的命令批准策略
        /// Updated command approval policy.
        #[serde(skip_serializing_if = "Option::is_none")]
        approval_policy: Option<AskForApproval>,

        /// 用于工具调用的更新沙盒策略
        /// Updated sandbox policy for tool calls.
        #[serde(skip_serializing_if = "Option::is_none")]
        sandbox_policy: Option<SandboxPolicy>,

        /// 更新的模型标识符。设置时，自动派生模型系列
        /// Updated model slug. When set, the model family is derived
        /// automatically.
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,

        /// 更新的推理努力（仅对具有推理能力的模型有效）
        /// Updated reasoning effort (honored only for reasoning-capable models).
        #[serde(skip_serializing_if = "Option::is_none")]
        effort: Option<ReasoningEffortConfig>,

        /// 更新的推理摘要偏好（仅对具有推理能力的模型有效）
        /// Updated reasoning summary preference (honored only for reasoning-capable models).
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<ReasoningSummaryConfig>,
    },

    /// Approve a command execution
    ExecApproval {
        /// The id of the submission we are approving
        id: String,
        /// The user's decision in response to the request.
        decision: ReviewDecision,
    },

    /// Approve a code patch
    PatchApproval {
        /// The id of the submission we are approving
        id: String,
        /// The user's decision in response to the request.
        decision: ReviewDecision,
    },

    /// Append an entry to the persistent cross-session message history.
    ///
    /// Note the entry is not guaranteed to be logged if the user has
    /// history disabled, it matches the list of "sensitive" patterns, etc.
    AddToHistory {
        /// The message text to be stored.
        text: String,
    },

    /// Request a single history entry identified by `log_id` + `offset`.
    GetHistoryEntryRequest { offset: usize, log_id: u64 },

    /// Request the full in-memory conversation transcript for the current session.
    /// Reply is delivered via `EventMsg::ConversationHistory`.
    GetHistory,

    /// Request the list of MCP tools available across all configured servers.
    /// Reply is delivered via `EventMsg::McpListToolsResponse`.
    ListMcpTools,

    /// Request the list of available custom prompts.
    ListCustomPrompts,

    /// Request the agent to summarize the current conversation context.
    /// The agent will use its existing context (either conversation history or previous response id)
    /// to generate a summary which will be returned as an AgentMessage event.
    Compact,
    /// Request to shut down codex instance.
    Shutdown,
}

/// 确定用户被咨询批准运行 Codex 提议的命令的条件
/// Determines the conditions under which the user is consulted to approve
/// running the command proposed by Codex.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, Display, TS)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum AskForApproval {
    /// 在此策略下，只有由 `is_safe_command()` 确定的"已知安全"命令
    /// 且**仅读取文件**的命令才会自动批准。其他所有命令都会要求用户批准
    /// Under this policy, only "known safe" commands—as determined by
    /// `is_safe_command()`—that **only read files** are auto‑approved.
    /// Everything else will ask the user to approve.
    #[serde(rename = "untrusted")]
    #[strum(serialize = "untrusted")]
    UnlessTrusted,

    /// *所有*命令都自动批准，但预期在沙盒内运行，网络访问被禁用，
    /// 写入被限制在特定路径集合内。如果命令失败，将升级为要求用户批准无沙盒执行
    /// *All* commands are auto‑approved, but they are expected to run inside a
    /// sandbox where network access is disabled and writes are confined to a
    /// specific set of paths. If the command fails, it will be escalated to
    /// the user to approve execution without a sandbox.
    OnFailure,

    /// 由模型决定何时向用户请求批准
    /// The model decides when to ask the user for approval.
    #[default]
    OnRequest,

    /// 从不要求用户批准命令。失败立即返回给模型，从不升级为要求用户批准
    /// Never ask the user to approve commands. Failures are immediately returned
    /// to the model, and never escalated to the user for approval.
    Never,
}

/// 确定模型 shell 命令的执行限制
/// Determines execution restrictions for model shell commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Display, TS)]
#[strum(serialize_all = "kebab-case")]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum SandboxPolicy {
    /// 完全没有限制。小心使用
    /// No restrictions whatsoever. Use with caution.
    #[serde(rename = "danger-full-access")]
    DangerFullAccess,

    /// 对整个文件系统的只读访问
    /// Read-only access to the entire file-system.
    #[serde(rename = "read-only")]
    ReadOnly,

    /// 与 `ReadOnly` 相同，但额外授予对当前工作目录（"工作空间"）的写访问权限
    /// Same as `ReadOnly` but additionally grants write access to the current
    /// working directory ("workspace").
    #[serde(rename = "workspace-write")]
    WorkspaceWrite {
        /// Additional folders (beyond cwd and possibly TMPDIR) that should be
        /// writable from within the sandbox.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        writable_roots: Vec<PathBuf>,

        /// When set to `true`, outbound network access is allowed. `false` by
        /// default.
        #[serde(default)]
        network_access: bool,

        /// When set to `true`, will NOT include the per-user `TMPDIR`
        /// environment variable among the default writable roots. Defaults to
        /// `false`.
        #[serde(default)]
        exclude_tmpdir_env_var: bool,

        /// When set to `true`, will NOT include the `/tmp` among the default
        /// writable roots on UNIX. Defaults to `false`.
        #[serde(default)]
        exclude_slash_tmp: bool,
    },
}

/// A writable root path accompanied by a list of subpaths that should remain
/// read‑only even when the root is writable. This is primarily used to ensure
/// top‑level VCS metadata directories (e.g. `.git`) under a writable root are
/// not modified by the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritableRoot {
    /// Absolute path, by construction.
    pub root: PathBuf,

    /// Also absolute paths, by construction.
    pub read_only_subpaths: Vec<PathBuf>,
}

impl WritableRoot {
    pub fn is_path_writable(&self, path: &Path) -> bool {
        // Check if the path is under the root.
        if !path.starts_with(&self.root) {
            return false;
        }

        // Check if the path is under any of the read-only subpaths.
        for subpath in &self.read_only_subpaths {
            if path.starts_with(subpath) {
                return false;
            }
        }

        true
    }
}

impl FromStr for SandboxPolicy {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

impl SandboxPolicy {
    /// 返回具有只读磁盘访问权限且无网络访问权限的策略
    /// Returns a policy with read-only disk access and no network.
    pub fn new_read_only_policy() -> Self {
        SandboxPolicy::ReadOnly
    }

    /// 返回可以读取整个磁盘但只能写入当前工作目录和 macOS 上每用户临时目录的策略。不允许网络访问
    /// Returns a policy that can read the entire disk, but can only write to
    /// the current working directory and the per-user tmp dir on macOS. It does
    /// not allow network access.
    pub fn new_workspace_write_policy() -> Self {
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: false,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        }
    }

    /// Always returns `true`; restricting read access is not supported.
    pub fn has_full_disk_read_access(&self) -> bool {
        true
    }

    pub fn has_full_disk_write_access(&self) -> bool {
        match self {
            SandboxPolicy::DangerFullAccess => true,
            SandboxPolicy::ReadOnly => false,
            SandboxPolicy::WorkspaceWrite { .. } => false,
        }
    }

    pub fn has_full_network_access(&self) -> bool {
        match self {
            SandboxPolicy::DangerFullAccess => true,
            SandboxPolicy::ReadOnly => false,
            SandboxPolicy::WorkspaceWrite { network_access, .. } => *network_access,
        }
    }

    /// Returns the list of writable roots (tailored to the current working
    /// directory) together with subpaths that should remain read‑only under
    /// each writable root.
    pub fn get_writable_roots_with_cwd(&self, cwd: &Path) -> Vec<WritableRoot> {
        match self {
            SandboxPolicy::DangerFullAccess => Vec::new(),
            SandboxPolicy::ReadOnly => Vec::new(),
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
                network_access: _,
            } => {
                // Start from explicitly configured writable roots.
                let mut roots: Vec<PathBuf> = writable_roots.clone();

                // Always include defaults: cwd, /tmp (if present on Unix), and
                // on macOS, the per-user TMPDIR unless explicitly excluded.
                roots.push(cwd.to_path_buf());

                // Include /tmp on Unix unless explicitly excluded.
                if cfg!(unix) && !exclude_slash_tmp {
                    let slash_tmp = PathBuf::from("/tmp");
                    if slash_tmp.is_dir() {
                        roots.push(slash_tmp);
                    }
                }

                // Include $TMPDIR unless explicitly excluded. On macOS, TMPDIR
                // is per-user, so writes to TMPDIR should not be readable by
                // other users on the system.
                //
                // By comparison, TMPDIR is not guaranteed to be defined on
                // Linux or Windows, but supporting it here gives users a way to
                // provide the model with their own temporary directory without
                // having to hardcode it in the config.
                if !exclude_tmpdir_env_var
                    && let Some(tmpdir) = std::env::var_os("TMPDIR")
                    && !tmpdir.is_empty()
                {
                    roots.push(PathBuf::from(tmpdir));
                }

                // For each root, compute subpaths that should remain read-only.
                roots
                    .into_iter()
                    .map(|writable_root| {
                        let mut subpaths = Vec::new();
                        let top_level_git = writable_root.join(".git");
                        if top_level_git.is_dir() {
                            subpaths.push(top_level_git);
                        }
                        WritableRoot {
                            root: writable_root,
                            read_only_subpaths: subpaths,
                        }
                    })
                    .collect()
            }
        }
    }
}

/// User input
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]

// 将用户输入（可能包含一个文本和多个图片），这里将其拆解为多个 InputItem
/// 用户输入项
/// User input
pub enum InputItem {
    /// 文本输入
    Text {
        text: String,
    },
    /// 预编码的数据 URI 图像
    /// Pre‑encoded data: URI image.
    Image {
        image_url: String,
    },

    /// 用户提供的本地图像路径。在请求序列化期间将被转换为 `Image` 变体（base64 数据 URL）
    /// Local image path provided by the user.  This will be converted to an
    /// `Image` variant (base64 data URL) during request serialization.
    LocalImage {
        path: std::path::PathBuf,
    },
}

/// 事件队列条目 - 来自代理的事件
/// Event Queue Entry - events from agent
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    /// 此事件关联的提交 `id`
    /// Submission `id` that this event is correlated with.
    pub id: String,
    /// 有效载荷
    /// Payload
    pub msg: EventMsg,
}

/// 来自代理的响应事件
/// Response event from the agent
#[derive(Debug, Clone, Deserialize, Serialize, Display)]
#[serde(tag = "type", rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum EventMsg {
    /// 执行提交时发生错误
    /// Error while executing a submission
    Error(ErrorEvent),

    /// 代理已开始任务
    /// Agent has started a task
    TaskStarted(TaskStartedEvent),

    /// 代理已完成所有操作
    /// Agent has completed all actions
    TaskComplete(TaskCompleteEvent),

    /// 令牌计数事件，定期发送以报告当前会话中使用的令牌数量
    /// Token count event, sent periodically to report the number of tokens
    /// used in the current session.
    TokenCount(TokenUsage),

    /// 代理文本输出消息
    /// Agent text output message
    AgentMessage(AgentMessageEvent),

    /// 代理文本输出增量消息
    /// Agent text output delta message
    AgentMessageDelta(AgentMessageDeltaEvent),

    /// Reasoning event from agent.
    AgentReasoning(AgentReasoningEvent),

    /// Agent reasoning delta event from agent.
    AgentReasoningDelta(AgentReasoningDeltaEvent),

    /// Raw chain-of-thought from agent.
    AgentReasoningRawContent(AgentReasoningRawContentEvent),

    /// Agent reasoning content delta event from agent.
    AgentReasoningRawContentDelta(AgentReasoningRawContentDeltaEvent),
    /// Signaled when the model begins a new reasoning summary section (e.g., a new titled block).
    AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent),

    /// Ack the client's configure message.
    SessionConfigured(SessionConfiguredEvent),

    McpToolCallBegin(McpToolCallBeginEvent),

    McpToolCallEnd(McpToolCallEndEvent),

    WebSearchBegin(WebSearchBeginEvent),

    WebSearchEnd(WebSearchEndEvent),

    /// Notification that the server is about to execute a command.
    ExecCommandBegin(ExecCommandBeginEvent),

    /// Incremental chunk of output from a running command.
    ExecCommandOutputDelta(ExecCommandOutputDeltaEvent),

    ExecCommandEnd(ExecCommandEndEvent),

    ExecApprovalRequest(ExecApprovalRequestEvent),

    ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent),

    BackgroundEvent(BackgroundEventEvent),

    /// Notification that a model stream experienced an error or disconnect
    /// and the system is handling it (e.g., retrying with backoff).
    StreamError(StreamErrorEvent),

    /// Notification that the agent is about to apply a code patch. Mirrors
    /// `ExecCommandBegin` so front‑ends can show progress indicators.
    PatchApplyBegin(PatchApplyBeginEvent),

    /// Notification that a patch application has finished.
    PatchApplyEnd(PatchApplyEndEvent),

    TurnDiff(TurnDiffEvent),

    /// Response to GetHistoryEntryRequest.
    GetHistoryEntryResponse(GetHistoryEntryResponseEvent),

    /// List of MCP tools available to the agent.
    McpListToolsResponse(McpListToolsResponseEvent),

    /// List of custom prompts available to the agent.
    ListCustomPromptsResponse(ListCustomPromptsResponseEvent),

    PlanUpdate(UpdatePlanArgs),

    TurnAborted(TurnAbortedEvent),

    /// Notification that the agent is shutting down.
    ShutdownComplete,

    ConversationHistory(ConversationHistoryResponseEvent),
}

// Individual event payload types matching each `EventMsg` variant.

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ErrorEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskCompleteEvent {
    pub last_agent_message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskStartedEvent {
    pub model_context_window: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: u64,
    pub reasoning_output_tokens: Option<u64>,
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn is_zero(&self) -> bool {
        self.total_tokens == 0
    }

    pub fn cached_input(&self) -> u64 {
        self.cached_input_tokens.unwrap_or(0)
    }

    pub fn non_cached_input(&self) -> u64 {
        self.input_tokens.saturating_sub(self.cached_input())
    }

    /// Primary count for display as a single absolute value: non-cached input + output.
    pub fn blended_total(&self) -> u64 {
        self.non_cached_input() + self.output_tokens
    }

    /// For estimating what % of the model's context window is used, we need to account
    /// for reasoning output tokens from prior turns being dropped from the context window.
    /// We approximate this here by subtracting reasoning output tokens from the total.
    /// This will be off for the current turn and pending function calls.
    pub fn tokens_in_context_window(&self) -> u64 {
        self.total_tokens
            .saturating_sub(self.reasoning_output_tokens.unwrap_or(0))
    }

    /// Estimate the remaining user-controllable percentage of the model's context window.
    ///
    /// `context_window` is the total size of the model's context window.
    /// `baseline_used_tokens` should capture tokens that are always present in
    /// the context (e.g., system prompt and fixed tool instructions) so that
    /// the percentage reflects the portion the user can influence.
    ///
    /// This normalizes both the numerator and denominator by subtracting the
    /// baseline, so immediately after the first prompt the UI shows 100% left
    /// and trends toward 0% as the user fills the effective window.
    pub fn percent_of_context_window_remaining(
        &self,
        context_window: u64,
        baseline_used_tokens: u64,
    ) -> u8 {
        if context_window <= baseline_used_tokens {
            return 0;
        }

        let effective_window = context_window - baseline_used_tokens;
        let used = self
            .tokens_in_context_window()
            .saturating_sub(baseline_used_tokens);
        let remaining = effective_window.saturating_sub(used);
        ((remaining as f32 / effective_window as f32) * 100.0).clamp(0.0, 100.0) as u8
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FinalOutput {
    pub token_usage: TokenUsage,
}

impl From<TokenUsage> for FinalOutput {
    fn from(token_usage: TokenUsage) -> Self {
        Self { token_usage }
    }
}

impl fmt::Display for FinalOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let token_usage = &self.token_usage;
        write!(
            f,
            "Token usage: total={} input={}{} output={}{}",
            token_usage.blended_total(),
            token_usage.non_cached_input(),
            if token_usage.cached_input() > 0 {
                format!(" (+ {} cached)", token_usage.cached_input())
            } else {
                String::new()
            },
            token_usage.output_tokens,
            token_usage
                .reasoning_output_tokens
                .map(|r| format!(" (reasoning {r})"))
                .unwrap_or_default()
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentMessageEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentMessageDeltaEvent {
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentReasoningEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentReasoningRawContentEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentReasoningRawContentDeltaEvent {
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentReasoningSectionBreakEvent {}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentReasoningDeltaEvent {
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpInvocation {
    /// Name of the MCP server as defined in the config.
    pub server: String,
    /// Name of the tool as given by the MCP server.
    pub tool: String,
    /// Arguments to the tool call.
    pub arguments: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpToolCallBeginEvent {
    /// Identifier so this can be paired with the McpToolCallEnd event.
    pub call_id: String,
    pub invocation: McpInvocation,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpToolCallEndEvent {
    /// Identifier for the corresponding McpToolCallBegin that finished.
    pub call_id: String,
    pub invocation: McpInvocation,
    pub duration: Duration,
    /// Result of the tool call. Note this could be an error.
    pub result: Result<CallToolResult, String>,
}

impl McpToolCallEndEvent {
    pub fn is_success(&self) -> bool {
        match &self.result {
            Ok(result) => !result.is_error.unwrap_or(false),
            Err(_) => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebSearchBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebSearchEndEvent {
    pub call_id: String,
    pub query: String,
}

/// Response payload for `Op::GetHistory` containing the current session's
/// in-memory transcript.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConversationHistoryResponseEvent {
    pub conversation_id: Uuid,
    pub entries: Vec<ResponseItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecCommandBeginEvent {
    /// Identifier so this can be paired with the ExecCommandEnd event.
    pub call_id: String,
    /// The command to be executed.
    pub command: Vec<String>,
    /// The command's working directory if not the default cwd for the agent.
    pub cwd: PathBuf,
    pub parsed_cmd: Vec<ParsedCommand>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecCommandEndEvent {
    /// Identifier for the ExecCommandBegin that finished.
    pub call_id: String,
    /// Captured stdout
    pub stdout: String,
    /// Captured stderr
    pub stderr: String,
    /// Captured aggregated output
    #[serde(default)]
    pub aggregated_output: String,
    /// The command's exit code.
    pub exit_code: i32,
    /// The duration of the command execution.
    pub duration: Duration,
    /// Formatted output from the command, as seen by the model.
    pub formatted_output: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecCommandOutputDeltaEvent {
    /// Identifier for the ExecCommandBegin that produced this chunk.
    pub call_id: String,
    /// Which stream produced this chunk.
    pub stream: ExecOutputStream,
    /// Raw bytes from the stream (may not be valid UTF-8).
    #[serde(with = "serde_bytes")]
    pub chunk: ByteBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecApprovalRequestEvent {
    /// Identifier for the associated exec call, if available.
    pub call_id: String,
    /// The command to be executed.
    pub command: Vec<String>,
    /// The command's working directory.
    pub cwd: PathBuf,
    /// Optional human-readable reason for the approval (e.g. retry without sandbox).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApplyPatchApprovalRequestEvent {
    /// Responses API call id for the associated patch apply call, if available.
    pub call_id: String,
    pub changes: HashMap<PathBuf, FileChange>,
    /// Optional explanatory reason (e.g. request for extra write access).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// When set, the agent is asking the user to allow writes under this root for the remainder of the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BackgroundEventEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StreamErrorEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PatchApplyBeginEvent {
    /// Identifier so this can be paired with the PatchApplyEnd event.
    pub call_id: String,
    /// If true, there was no ApplyPatchApprovalRequest for this patch.
    pub auto_approved: bool,
    /// The changes to be applied.
    pub changes: HashMap<PathBuf, FileChange>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PatchApplyEndEvent {
    /// Identifier for the PatchApplyBegin that finished.
    pub call_id: String,
    /// Captured stdout (summary printed by apply_patch).
    pub stdout: String,
    /// Captured stderr (parser errors, IO failures, etc.).
    pub stderr: String,
    /// Whether the patch was applied successfully.
    pub success: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TurnDiffEvent {
    pub unified_diff: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GetHistoryEntryResponseEvent {
    pub offset: usize,
    pub log_id: u64,
    /// The entry at the requested offset, if available and parseable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<HistoryEntry>,
}

/// Response payload for `Op::ListMcpTools`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpListToolsResponseEvent {
    /// Fully qualified tool name -> tool definition.
    pub tools: std::collections::HashMap<String, McpTool>,
}

/// Response payload for `Op::ListCustomPrompts`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ListCustomPromptsResponseEvent {
    pub custom_prompts: Vec<CustomPrompt>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct SessionConfiguredEvent {
    /// Unique id for this session.
    pub session_id: Uuid,

    /// Tell the client what model is being queried.
    pub model: String,

    /// Identifier of the history log file (inode on Unix, 0 otherwise).
    pub history_log_id: u64,

    /// Current number of entries in the history log.
    pub history_entry_count: usize,
}

/// User's decision in response to an ExecApprovalRequest.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// User has approved this command and the agent should execute it.
    Approved,

    /// User has approved this command and wants to automatically approve any
    /// future identical instances (`command` and `cwd` match exactly) for the
    /// remainder of the session.
    ApprovedForSession,

    /// User has denied this command and the agent should not execute it, but
    /// it should continue the session and try something else.
    #[default]
    Denied,

    /// User has denied this command and the agent should not do anything until
    /// the user's next command.
    Abort,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, TS)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    Add {
        content: String,
    },
    Delete,
    Update {
        unified_diff: String,
        move_path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Chunk {
    /// 1-based line index of the first line in the original file
    pub orig_index: u32,
    pub deleted_lines: Vec<String>,
    pub inserted_lines: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TurnAbortedEvent {
    pub reason: TurnAbortReason,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, TS)]
#[serde(rename_all = "snake_case")]
pub enum TurnAbortReason {
    Interrupted,
    Replaced,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize Event to verify that its JSON representation has the expected
    /// amount of nesting.
    #[test]
    fn serialize_event() {
        let session_id: Uuid = uuid::uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8");
        let event = Event {
            id: "1234".to_string(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id,
                model: "codex-mini-latest".to_string(),
                history_log_id: 0,
                history_entry_count: 0,
            }),
        };
        let serialized = serde_json::to_string(&event).unwrap();
        assert_eq!(
            serialized,
            r#"{"id":"1234","msg":{"type":"session_configured","session_id":"67e55044-10b1-426f-9247-bb680e5fe0c8","model":"codex-mini-latest","history_log_id":0,"history_entry_count":0}}"#
        );
    }
}
