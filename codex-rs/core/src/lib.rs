//! Root of the `codex-core` library.
//!
//! 说明（中文注释）:
//! - 这是 codex 项目中被多个二进制（例如 `codex`/`codex-tui`/`codex-exec`）共同依赖的核心库。
//! - 本文件是 crate 的入口（crate root），通常放置公共模块声明与对外导出（`pub use`）。
//! - 我会在文件中用中文注释说明每个模块的大致职责，便于阅读调用链。
//!
//! 快速的 Rust 语法说明（阅读时参考）:
//! - `mod foo;`：将同目录下的 `foo.rs` 或 `foo/mod.rs` 作为子模块包含进来（模块并未默认导出）；
//! - `pub mod foo;`：同时把该模块对外导出（其他 crate 可以通过 `codex_core::foo::...` 访问）；
//! - `pub use path::Type;`：把某个路径下的类型或函数重新导出（re-export），便于外部直接使用 `codex_core::Type`。
//! - crate 属性（如 `#![deny(...)]`）在 crate 层面控制 lint/编译行为。
// Prevent accidental direct writes to stdout/stderr in library code. All
// user-visible output must go through the appropriate abstraction (e.g.,
// the TUI or the tracing stack).
#![deny(clippy::print_stdout, clippy::print_stderr)]

// -----------------------
// 模块声明与导出区域
// -----------------------
// 下面的 `mod` / `pub mod` 声明把实现拆分到不同文件中。注：`mod x;` 把模块包含进来，
// 但并不对外导出；若希望其他 crate 使用，需要使用 `pub mod` 或者 `pub use` 重新导出类型。
mod apply_patch; // 负责将 agent 生成的补丁应用到工作区（验证、写盘、调用 git apply 等）
mod bash; // 与 shell/命令相关的辅助代码
mod chat_completions; // 与模型聊天补全（chat completions）相关的 glue 代码
mod client; // 与外部服务交互的客户端包装（可能包含 HTTP 调用等）
mod client_common; // client 的共用工具
pub mod codex; // 对外公开的高层 API（可能包含较为稳定的抽象）
mod codex_conversation; // 会话（conversation）逻辑实现
pub use codex_conversation::CodexConversation; // 重新导出便于上层调用者直接使用
pub mod config; // 配置加载与解析
pub mod config_profile; // 配置 profile（多套配置）
pub mod config_types; // 配置相关的类型定义
mod conversation_history; // 会话历史的持久化与读取
pub mod custom_prompts; // 自定义 prompt 管理
mod environment_context; // 运行时环境相关的上下文（cwd、env 等）
pub mod error; // 错误类型与处理工具
pub mod exec; // 执行/运行命令的高级封装
mod exec_command; // 低层 exec 命令实现
pub mod exec_env; // exec 相关的环境管理（沙箱、路径等）
mod flags; // CLI/运行时标志解析辅助
pub mod git_info; // 与 git 仓库元信息相关的工具
mod is_safe_command; // 判断命令是否安全（用于 sandbox 策略）
pub mod landlock; // Linux landlock 相关封装（如果支持）
mod mcp_connection_manager; // MCP 连接管理
mod mcp_tool_call; // MCP 工具调用封装
mod message_history; // 消息历史（可能与 conversation_history 有区别）
mod model_provider_info; // 模型提供者信息与注册
pub mod parse_command; // 将用户/agent 的文本解析为可执行命令的工具
// 下面几行把 model_provider_info 中的一些常用常量/类型对外导出，方便调用端写 `codex_core::ModelProviderInfo`。
pub use model_provider_info::BUILT_IN_OSS_MODEL_PROVIDER_ID;
pub use model_provider_info::ModelProviderInfo;
pub use model_provider_info::WireApi;
pub use model_provider_info::built_in_model_providers;
pub use model_provider_info::create_oss_provider_with_base_url;
mod conversation_manager; // 会话管理器（新会话、会话切换等）
pub use conversation_manager::ConversationManager; // 重新导出
pub use conversation_manager::NewConversation; // 新会话构造器
pub mod model_family; // 模型家族/分组相关类型
mod openai_model_info; // OpenAI 模型相关的硬编码或映射表
mod openai_tools; // OpenAI 特有工具的包装
pub mod plan_tool; // 计划工具（由模型生成执行步骤）
pub mod project_doc; // 项目文档（AGENTS.md 等）解析
mod rollout; // rollout/特性开关等
pub(crate) mod safety; // crate 私有的安全工具（仅在 core 内可见）
pub mod seatbelt; // macOS Seatbelt sandbox 集成
pub mod shell; // shell 交互封装
pub mod spawn; // spawn 子进程工具
pub mod terminal; // 终端相关抽象（例如处理 tty）
mod tool_apply_patch; // 作为工具调用时的 apply_patch glue
pub mod turn_diff_tracker; // 跟踪 turn（agent 轮次）的 diff
pub mod user_agent; // 用户 agent 相关类型/逻辑
mod user_notification; // 用户通知（桌面通知等）
pub mod util; // 通用工具函数
// 下面是对外常量与工具函数导出
pub use apply_patch::CODEX_APPLY_PATCH_ARG1;
pub use safety::get_platform_sandbox;

// Re-export the protocol types from the standalone `codex-protocol` crate so existing
// `codex_core::protocol::...` references continue to work across the workspace.
// 说明：`codex_protocol` 是一个独立的 crate（通常包含消息的 serde 类型），
// 通过 `pub use` 把它的 module 暴露到 codex_core 的命名空间下，方便上层直接使用。
pub use codex_protocol::protocol;
// Re-export protocol config enums to ensure call sites can use the same types
// as those in the protocol crate when constructing protocol messages.
pub use codex_protocol::config_types as protocol_config_types;
