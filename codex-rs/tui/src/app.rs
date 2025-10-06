//! Codex CLI 应用主协调器
//!
//! 说明 (中文注释):
//! - `App` 是 TUI 应用的顶层控制器，负责协调所有组件间的交互
//! - 管理应用级事件循环、组件生命周期和状态同步
//! - 处理用户输入、AI 响应、文件操作等核心业务流程
//! - 实现会话回退 (backtrack) 功能和覆盖层 (overlay) 管理

use crate::app_backtrack::BacktrackState;          // 回退状态管理
use crate::app_event::AppEvent;                    // 应用级事件定义
use crate::app_event_sender::AppEventSender;      // 事件发送器封装
use crate::chatwidget::ChatWidget;                 // 主聊天界面组件
use crate::file_search::FileSearchManager;        // 文件搜索管理器
use crate::pager_overlay::Overlay;                 // 覆盖层组件 (如会话记录查看器)
use crate::tui;                                    // TUI 基础设施
use crate::tui::TuiEvent;                          // 终端UI事件
use codex_ansi_escape::ansi_escape_line;          // ANSI 转义序列处理
use codex_core::ConversationManager;              // 会话管理器
use codex_core::config::Config;                   // 配置管理
use codex_core::protocol::TokenUsage;             // Token 使用统计
use codex_login::AuthManager;                     // 认证管理器
use color_eyre::eyre::Result;                     // 错误处理
use crossterm::event::KeyCode;                    // 按键码定义
use crossterm::event::KeyEvent;                   // 按键事件
use crossterm::event::KeyEventKind;               // 按键事件类型
use crossterm::terminal::supports_keyboard_enhancement; // 键盘增强功能检测
use ratatui::style::Stylize;                      // 样式化工具
use ratatui::text::Line;                          // 文本行
use std::path::PathBuf;                           // 路径处理
use std::sync::Arc;                               // 原子引用计数
use std::sync::atomic::AtomicBool;                // 原子布尔值
use std::sync::atomic::Ordering;                  // 内存排序
use std::thread;                                  // 线程支持
use std::time::Duration;                          // 时间间隔
use tokio::select;                                // 异步选择宏
use tokio::sync::mpsc::unbounded_channel;         // 无界消息通道
// use uuid::Uuid;

/// App 结构体 - Codex CLI 应用的主控制器
///
/// 负责协调所有子组件，管理应用状态和事件分发
pub(crate) struct App {
    /// 会话管理器 - 管理多个AI对话会话的生命周期
    pub(crate) server: Arc<ConversationManager>,

    /// 应用事件发送器 - 用于组件间异步通信
    pub(crate) app_event_tx: AppEventSender,

    /// 主聊天组件 - 负责用户交互和消息显示
    pub(crate) chat_widget: ChatWidget,

    /// 应用配置 - 存储在此处以便在需要时重新创建 ChatWidget
    pub(crate) config: Config,

    /// 文件搜索管理器 - 处理 @文件名 语法的文件搜索功能
    pub(crate) file_search: FileSearchManager,

    /// 会话记录行 - 存储完整的对话历史记录，用于会话记录查看器
    pub(crate) transcript_lines: Vec<Line<'static>>,

    /// 覆盖层状态 - 可选的全屏覆盖层 (如会话记录查看器或静态内容如Diff)
    pub(crate) overlay: Option<Overlay>,

    /// 延迟历史记录行 - 当覆盖层活跃时暂存的历史记录，关闭覆盖层后会被应用
    pub(crate) deferred_history_lines: Vec<Line<'static>>,

    /// 终端是否支持键盘增强功能 - 影响快捷键的可用性
    pub(crate) enhanced_keys_supported: bool,

    /// 动画线程控制器 - 控制发送 CommitTick 事件的动画线程
    /// 用于流式文本显示的逐字动画效果
    pub(crate) commit_anim_running: Arc<AtomicBool>,

    /// Esc键回退功能状态 - 实现 Esc-Esc 快捷键回退到对话历史的功能
    pub(crate) backtrack: crate::app_backtrack::BacktrackState,
}

impl App {
    /// 应用主入口函数 - 启动 Codex CLI 应用
    ///
    /// 参数说明:
    /// - `tui`: TUI管理器引用，负责终端界面的底层操作
    /// - `auth_manager`: 认证管理器，处理用户登录和API密钥
    /// - `config`: 应用配置，包含模型、沙箱策略等设置
    /// - `initial_prompt`: 可选的初始提示词，应用启动时自动发送
    /// - `initial_images`: 初始图片附件列表
    ///
    /// 返回值: 应用退出时的总Token使用量统计
    pub async fn run(
        tui: &mut tui::Tui,
        auth_manager: Arc<AuthManager>,
        config: Config,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
    ) -> Result<TokenUsage> {
        use tokio_stream::StreamExt;

        // 创建应用事件通道 - 用于组件间异步通信
        let (app_event_tx, mut app_event_rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);

        // 初始化会话管理器 - 管理与AI模型的对话会话
        let conversation_manager = Arc::new(ConversationManager::new(auth_manager.clone()));

        // 检测终端是否支持键盘增强功能 (如 Shift+Enter)
        let enhanced_keys_supported = supports_keyboard_enhancement().unwrap_or(false);

        // 创建主聊天组件 - 应用的核心交互界面
        let chat_widget = ChatWidget::new(
            config.clone(),
            conversation_manager.clone(),
            tui.frame_requester(),
            app_event_tx.clone(),
            initial_prompt,
            initial_images,
            enhanced_keys_supported,
        );

        // 初始化文件搜索管理器 - 处理 @文件名 搜索功能
        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());

        // 构建应用实例
        let mut app = Self {
            server: conversation_manager,
            app_event_tx,
            chat_widget,
            config,
            file_search,
            enhanced_keys_supported,
            transcript_lines: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
        };

        // 获取TUI事件流 - 处理键盘输入、鼠标事件等
        let tui_events = tui.event_stream();
        tokio::pin!(tui_events);

        // 请求首次渲染
        tui.frame_requester().schedule_frame();

        // 主事件循环 - 同时监听应用事件和TUI事件
        while select! {
            // 处理应用内部事件 (如AI响应、文件搜索结果等)
            Some(event) = app_event_rx.recv() => {
                app.handle_event(tui, event).await?
            }
            // 处理用户输入事件 (按键、粘贴、绘制等)
            Some(event) = tui_events.next() => {
                app.handle_tui_event(tui, event).await?
            }
        } {}

        // 应用退出时清理终端状态
        tui.terminal.clear()?;
        Ok(app.token_usage())
    }

    /// 处理TUI事件 - 包括用户输入、绘制请求等终端界面事件
    ///
    /// 参数说明:
    /// - `tui`: TUI管理器的可变引用
    /// - `event`: 终端事件 (按键、粘贴、绘制、图片附加等)
    ///
    /// 返回值: Ok(true) 继续运行, Ok(false) 退出应用, Err 发生错误
    pub(crate) async fn handle_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        event: TuiEvent,
    ) -> Result<bool> {
        // 如果当前有覆盖层激活 (如会话记录查看器)，优先处理覆盖层事件
        if self.overlay.is_some() {
            let _ = self.handle_backtrack_overlay_event(tui, event).await?;
        } else {
            // 普通模式下的事件处理
            match event {
                // 键盘输入事件
                TuiEvent::Key(key_event) => {
                    self.handle_key_event(tui, key_event).await;
                }
                // 文本粘贴事件
                TuiEvent::Paste(pasted) => {
                    // 许多终端在粘贴时会将换行符转换为 \r (如 iTerm2)，
                    // 但 tui-textarea 期望的是 \n。统一转换 CR 为 LF。
                    // 参考: tui-textarea 和 iTerm2 的相关实现
                    let pasted = pasted.replace("\r", "\n");
                    self.chat_widget.handle_paste(pasted);
                }
                // 绘制事件 - 渲染界面
                TuiEvent::Draw => {
                    // 处理粘贴突发检测的定时器，如果正在处理粘贴突发则跳过本次绘制
                    if self
                        .chat_widget
                        .handle_paste_burst_tick(tui.frame_requester())
                    {
                        return Ok(true);
                    }
                    // 执行界面渲染
                    tui.draw(
                        self.chat_widget.desired_height(tui.terminal.size()?.width),
                        |frame| {
                            // 渲染主聊天组件
                            frame.render_widget_ref(&self.chat_widget, frame.area());
                            // 设置光标位置 (如果聊天组件返回了光标坐标)
                            if let Some((x, y)) = self.chat_widget.cursor_pos(frame.area()) {
                                frame.set_cursor_position((x, y));
                            }
                        },
                    )?;
                }
                // 图片附加事件 - 用户拖拽或粘贴图片
                TuiEvent::AttachImage {
                    path,
                    width,
                    height,
                    format_label,
                } => {
                    self.chat_widget
                        .attach_image(path, width, height, format_label);
                }
            }
        }
        Ok(true)
    }

    /// 处理应用事件 - 处理组件间的内部通信事件
    ///
    /// 参数说明:
    /// - `tui`: TUI管理器的可变引用
    /// - `event`: 应用级事件 (AI响应、用户操作、系统事件等)
    ///
    /// 返回值: Ok(true) 继续运行, Ok(false) 退出应用, Err 发生错误
    async fn handle_event(&mut self, tui: &mut tui::Tui, event: AppEvent) -> Result<bool> {
        match event {
            // 新建会话事件 - 用户请求创建新的对话会话
            AppEvent::NewSession => {
                self.chat_widget = ChatWidget::new(
                    self.config.clone(),
                    self.server.clone(),
                    tui.frame_requester(),
                    self.app_event_tx.clone(),
                    None,           // 没有初始提示词
                    Vec::new(),     // 没有初始图片
                    self.enhanced_keys_supported,
                );
                tui.frame_requester().schedule_frame();
            }
            // 插入历史记录行事件 - 向对话历史中添加文本行
            AppEvent::InsertHistoryLines(lines) => {
                // 如果会话记录覆盖层是活跃的，同时更新覆盖层内容
                if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                    t.insert_lines(lines.clone());
                    tui.frame_requester().schedule_frame();
                }
                // 更新完整的会话记录
                self.transcript_lines.extend(lines.clone());
                // 如果有覆盖层激活，将显示内容推迟到覆盖层关闭后
                if self.overlay.is_some() {
                    self.deferred_history_lines.extend(lines);
                } else {
                    // 立即显示在主界面
                    tui.insert_history_lines(lines);
                }
            }
            // 插入历史记录单元事件 - 向对话历史中添加结构化的历史单元
            AppEvent::InsertHistoryCell(cell) => {
                // 获取单元的会话记录表示 (用于会话记录查看器)
                let cell_transcript = cell.transcript_lines();
                if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                    t.insert_lines(cell_transcript.clone());
                    tui.frame_requester().schedule_frame();
                }
                self.transcript_lines.extend(cell_transcript.clone());

                // 获取单元的显示表示 (用于主界面显示)
                let display = cell.display_lines();
                if !display.is_empty() {
                    if self.overlay.is_some() {
                        self.deferred_history_lines.extend(display);
                    } else {
                        tui.insert_history_lines(display);
                    }
                }
            }
            // 启动流式显示动画事件 - 开始逐字显示AI响应的动画效果
            AppEvent::StartCommitAnimation => {
                // 使用原子操作确保只有一个动画线程在运行
                if self
                    .commit_anim_running
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let tx = self.app_event_tx.clone();
                    let running = self.commit_anim_running.clone();
                    // 在新线程中运行动画定时器
                    thread::spawn(move || {
                        while running.load(Ordering::Relaxed) {
                            thread::sleep(Duration::from_millis(50));
                            tx.send(AppEvent::CommitTick);
                        }
                    });
                }
            }
            // 停止流式显示动画事件
            AppEvent::StopCommitAnimation => {
                self.commit_anim_running.store(false, Ordering::Release);
            }
            // 动画定时器事件 - 触发一次流式文本的提交显示
            AppEvent::CommitTick => {
                self.chat_widget.on_commit_tick();
            }
            // Codex核心事件 - 来自AI模型或命令执行的事件
            AppEvent::CodexEvent(event) => {
                self.chat_widget.handle_codex_event(event);
            }
            // 对话历史事件 - 用于实现会话回退功能
            AppEvent::ConversationHistory(ev) => {
                self.on_conversation_history_for_backtrack(tui, ev).await?;
            }
            // 退出请求事件 - 用户请求退出应用
            AppEvent::ExitRequest => {
                return Ok(false);
            }
            // Codex操作事件 - 向Codex核心发送操作指令
            AppEvent::CodexOp(op) => self.chat_widget.submit_op(op),
            // Diff结果事件 - 显示git diff的结果
            AppEvent::DiffResult(text) => {
                // 清除底部面板的"正在处理"状态
                self.chat_widget.on_diff_complete();
                // 进入备用屏幕模式以显示全屏内容
                let _ = tui.enter_alt_screen();
                // 构建分页器显示的内容行
                let pager_lines: Vec<ratatui::text::Line<'static>> = if text.trim().is_empty() {
                    vec!["No changes detected.".italic().into()]
                } else {
                    // 处理ANSI转义序列以正确显示颜色和格式
                    text.lines().map(ansi_escape_line).collect()
                };
                // 创建静态覆盖层显示diff内容
                self.overlay = Some(Overlay::new_static_with_title(
                    pager_lines,
                    "D I F F".to_string(),
                ));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::StartFileSearch(query) => {
                if !query.is_empty() {
                    self.file_search.on_user_query(query);
                }
            }
            AppEvent::FileSearchResult { query, matches } => {
                self.chat_widget.apply_file_search_result(query, matches);
            }
            AppEvent::UpdateReasoningEffort(effort) => {
                self.chat_widget.set_reasoning_effort(effort);
            }
            AppEvent::UpdateModel(model) => {
                self.chat_widget.set_model(model);
            }
            AppEvent::UpdateAskForApprovalPolicy(policy) => {
                self.chat_widget.set_approval_policy(policy);
            }
            AppEvent::UpdateSandboxPolicy(policy) => {
                self.chat_widget.set_sandbox_policy(policy);
            }
        }
        Ok(true)
    }

    /// 获取当前的Token使用统计
    pub(crate) fn token_usage(&self) -> codex_core::protocol::TokenUsage {
        self.chat_widget.token_usage().clone()
    }

    /// 处理键盘输入事件 - 处理应用级快捷键和会话回退逻辑
    ///
    /// 参数说明:
    /// - `tui`: TUI管理器的可变引用
    /// - `key_event`: 键盘事件详情
    async fn handle_key_event(&mut self, tui: &mut tui::Tui, key_event: KeyEvent) {
        match key_event {
            // Ctrl+T: 打开会话记录查看器
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } => {
                // 进入备用屏幕模式并设置视口为全尺寸
                let _ = tui.enter_alt_screen();
                self.overlay = Some(Overlay::new_transcript(self.transcript_lines.clone()));
                tui.frame_requester().schedule_frame();
            }
            // Esc键: 实现会话回退功能的核心逻辑
            // 只有在正常模式 (非工作状态) 且输入框为空时才启动/推进回退
            // 其他情况下将Esc转发给活跃的UI组件 (如状态指示器、模态框、弹窗) 处理
            KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                if self.chat_widget.is_normal_backtrack_mode()
                    && self.chat_widget.composer_is_empty()
                {
                    self.handle_backtrack_esc_key(tui);
                } else {
                    self.chat_widget.handle_key_event(key_event);
                }
            }
            // Enter键: 当回退已准备就绪且计数大于0时确认回退
            // 其他情况下传递给组件处理
            KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            } if self.backtrack.primed
                && self.backtrack.count > 0
                && self.chat_widget.composer_is_empty() =>
            {
                // 委托给辅助方法以保持代码清晰；保留原有行为
                self.confirm_backtrack_from_main();
            }
            // 其他按键: 处理一般的键盘输入
            KeyEvent {
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            } => {
                // 任何非Esc按键都应该取消已准备的回退状态
                // 这避免了用户开始输入后出现过时的"Esc已准备"状态
                // (即使用户后来退格到空白状态)
                if key_event.code != KeyCode::Esc && self.backtrack.primed {
                    self.reset_backtrack_state();
                }
                self.chat_widget.handle_key_event(key_event);
            }
            _ => {
                // 忽略按键释放事件
            }
        };
    }
}
