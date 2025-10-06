//! 会话管理器模块
//!
//! 此模块实现了 Codex 系统的会话生命周期管理，负责：
//! 1. 创建和销毁 AI 对话会话
//! 2. 管理多个并发会话的状态
//! 3. 处理会话分叉（fork）功能，用于实现对话历史回退
//! 4. 维护会话与认证的关联关系
//!
//! 核心设计模式：
//! - 管理器模式：统一管理所有会话的生命周期
//! - 工厂模式：负责创建配置好的会话实例
//! - 共享状态：使用 Arc + RwLock 实现线程安全的会话共享

use std::collections::HashMap; // 用于存储会话ID到会话实例的映射
use std::sync::Arc; // 原子引用计数，实现安全的跨线程共享

use codex_login::AuthManager; // 认证管理器，处理用户登录状态
use codex_login::CodexAuth; // 认证信息结构体
use tokio::sync::RwLock; // 异步读写锁，保护会话映射表
use uuid::Uuid; // UUID生成器，用于会话唯一标识

use crate::codex::Codex; // 核心Codex接口
use crate::codex::CodexSpawnOk; // Codex创建成功的返回结构
use crate::codex::INITIAL_SUBMIT_ID; // 初始提交ID常量
use crate::codex_conversation::CodexConversation; // 会话包装器
use crate::config::Config; // 系统配置
use crate::error::CodexErr; // 错误类型定义
use crate::error::Result as CodexResult; // 结果类型别名
use crate::protocol::Event; // 事件消息类型
use crate::protocol::EventMsg; // 事件消息内容
use crate::protocol::SessionConfiguredEvent; // 会话配置完成事件
use codex_protocol::models::ResponseItem; // 响应项模型

/// Represents a newly created Codex conversation, including the first event
/// (which is [`EventMsg::SessionConfigured`]).
///
/// 新创建会话的返回结构
/// 包含会话ID、会话实例和初始配置事件，用于向调用者提供完整的会话初始化信息
pub struct NewConversation {
    pub conversation_id: Uuid,                      // 会话唯一标识符
    pub conversation: Arc<CodexConversation>,       // 会话实例的原子引用，可跨线程安全共享
    pub session_configured: SessionConfiguredEvent, // 会话配置完成事件，包含初始化参数
}

/// [`ConversationManager`] is responsible for creating conversations and
/// maintaining them in memory.
///
/// 会话管理器核心结构
/// 采用管理器模式统一管理所有活跃会话的生命周期，提供线程安全的会话创建、获取、删除和分叉功能
pub struct ConversationManager {
    conversations: Arc<RwLock<HashMap<Uuid, Arc<CodexConversation>>>>, // 会话映射表，使用读写锁保护的HashMap存储会话ID到会话实例的映射
    auth_manager: Arc<AuthManager>, // 认证管理器，负责处理用户身份验证和授权
}

impl ConversationManager {
    /// 创建新的会话管理器实例
    /// 初始化空的会话映射表和传入的认证管理器
    pub fn new(auth_manager: Arc<AuthManager>) -> Self {
        Self {
            conversations: Arc::new(RwLock::new(HashMap::new())), // 创建空的线程安全会话映射表
            auth_manager,                                         // 保存认证管理器引用
        }
    }

    /// Construct with a dummy AuthManager containing the provided CodexAuth.
    /// Used for integration tests: should not be used by ordinary business logic.
    ///
    /// 使用指定认证信息创建会话管理器（测试专用）
    /// 创建一个包含虚拟认证管理器的会话管理器实例，仅用于集成测试，不应在正常业务逻辑中使用
    pub fn with_auth(auth: CodexAuth) -> Self {
        Self::new(codex_login::AuthManager::from_auth_for_testing(auth)) // 使用测试专用方法创建认证管理器
    }

    /// 创建新的会话
    /// 使用提供的配置和当前认证管理器创建一个新的AI对话会话
    pub async fn new_conversation(&self, config: Config) -> CodexResult<NewConversation> {
        self.spawn_conversation(config, self.auth_manager.clone()) // 调用内部spawn方法创建会话
            .await
    }

    /// 内部会话创建方法
    /// 负责实际的Codex实例创建，这是整个代码库中唯一调用Codex::spawn的地方
    /// 体现了"codex is owned only by conversation"的架构设计
    async fn spawn_conversation(
        &self,
        config: Config,                 // 会话配置参数
        auth_manager: Arc<AuthManager>, // 认证管理器
    ) -> CodexResult<NewConversation> {
        let CodexSpawnOk {
            codex,                       // 创建的Codex核心实例
            session_id: conversation_id, // 会话ID（重命名为conversation_id以符合语义）
        } = {
            let initial_history = None; // 初始对话历史为空（新会话）
            Codex::spawn(config, auth_manager, initial_history).await? // 调用Codex::spawn创建核心实例
        };
        self.finalize_spawn(codex, conversation_id).await // 完成会话初始化流程
    }

    /// 完成会话创建的最终步骤
    /// 验证首个事件、封装会话实例、注册到管理器并返回完整的新会话信息
    async fn finalize_spawn(
        &self,
        codex: Codex,          // 已创建的Codex实例
        conversation_id: Uuid, // 会话唯一标识符
    ) -> CodexResult<NewConversation> {
        // The first event must be `SessionInitialized`. Validate and forward it
        // to the caller so that they can display it in the conversation
        // history.
        // 等待并验证首个事件必须是SessionConfigured
        let event = codex.next_event().await?; // 获取Codex的首个事件
        let session_configured = match event {
            Event {
                id,
                msg: EventMsg::SessionConfigured(session_configured), // 匹配会话配置完成事件
            } if id == INITIAL_SUBMIT_ID => session_configured, // 验证事件ID为初始提交ID
            _ => {
                return Err(CodexErr::SessionConfiguredNotFirstEvent); // 首个事件不是SessionConfigured时返回错误
            }
        };

        let conversation = Arc::new(CodexConversation::new(codex)); // 将Codex封装为CodexConversation
        self.conversations
            .write() // 获取会话映射表的写锁
            .await
            .insert(conversation_id, conversation.clone()); // 将新会话注册到管理器

        Ok(NewConversation {
            conversation_id,    // 返回会话ID
            conversation,       // 返回会话实例
            session_configured, // 返回初始配置事件
        })
    }

    /// 根据ID获取已存在的会话
    /// 从会话映射表中查找指定ID的会话实例，如果不存在则返回错误
    pub async fn get_conversation(
        &self,
        conversation_id: Uuid, // 要查找的会话ID
    ) -> CodexResult<Arc<CodexConversation>> {
        let conversations = self.conversations.read().await; // 获取会话映射表的读锁
        conversations
            .get(&conversation_id) // 在映射表中查找会话
            .cloned() // 克隆Arc引用（增加引用计数）
            .ok_or_else(|| CodexErr::ConversationNotFound(conversation_id)) // 未找到时返回ConversationNotFound错误
    }

    /// 从管理器中移除指定会话
    /// 从会话映射表中删除指定ID的会话，会话实例的生命周期由Arc引用计数管理
    pub async fn remove_conversation(&self, conversation_id: Uuid) {
        // 要移除的会话ID
        self.conversations.write().await.remove(&conversation_id); // 获取写锁并从映射表中移除会话
    }

    /// Fork an existing conversation by dropping the last `drop_last_messages`
    /// user/assistant messages from its transcript and starting a new
    /// conversation with identical configuration (unless overridden by the
    /// caller's `config`). The new conversation will have a fresh id.
    ///
    /// 分叉现有会话功能
    /// 通过删除最后N条用户/助手消息来截断对话历史，然后基于截断后的历史创建新会话
    /// 这个功能用于实现对话历史回退，让用户可以从之前的某个时点重新开始对话
    pub async fn fork_conversation(
        &self,
        conversation_history: Vec<ResponseItem>, // 原会话的完整对话历史
        num_messages_to_drop: usize,             // 要删除的最后N条消息数量
        config: Config,                          // 新会话的配置（可覆盖原配置）
    ) -> CodexResult<NewConversation> {
        // Compute the prefix up to the cut point.
        // 计算截断点，生成新的对话历史前缀
        let truncated_history =
            truncate_after_dropping_last_messages(conversation_history, num_messages_to_drop);

        // Spawn a new conversation with the computed initial history.
        // 使用计算出的初始历史创建新会话
        let auth_manager = self.auth_manager.clone(); // 复用当前的认证管理器
        let CodexSpawnOk {
            codex,                       // 新创建的Codex实例
            session_id: conversation_id, // 新会话ID
        } = Codex::spawn(config, auth_manager, Some(truncated_history)).await?; // 传入截断后的历史作为初始历史

        self.finalize_spawn(codex, conversation_id).await // 完成新会话的初始化
    }
}

/// Return a prefix of `items` obtained by dropping the last `n` user messages
/// and all items that follow them.
///
/// 截断对话历史的工具函数
/// 从对话项列表中删除最后N条用户消息及其后续的所有内容，返回截断后的前缀
/// 只计算用户消息，不计算助手消息或其他类型的响应项
fn truncate_after_dropping_last_messages(items: Vec<ResponseItem>, n: usize) -> Vec<ResponseItem> {
    if n == 0 || items.is_empty() {
        // 如果不需要删除或列表为空，直接返回原列表
        return items;
    }

    // Walk backwards counting only `user` Message items, find cut index.
    // 从后向前遍历，只计算用户消息，找到截断索引
    let mut count = 0usize; // 已找到的用户消息计数
    let mut cut_index = 0usize; // 截断位置索引
    for (idx, item) in items.iter().enumerate().rev() {
        // 逆序遍历所有响应项
        if let ResponseItem::Message { role, .. } = item
            && role == "user"
        // 只匹配用户角色的消息
        {
            count += 1; // 用户消息计数加1
            if count == n {
                // 达到目标删除数量
                // Cut everything from this user message to the end.
                // 从这条用户消息开始截断到末尾
                cut_index = idx;
                break;
            }
        }
    }
    if count < n {
        // 如果用户消息总数少于要删除的数量
        // If fewer than n messages exist, drop everything.
        // 删除所有内容，返回空列表
        Vec::new()
    } else {
        items.into_iter().take(cut_index).collect() // 保留截断索引之前的所有项
    }
}

#[cfg(test)]
mod tests {
    // 测试模块，验证会话管理器功能
    use super::*;
    use codex_protocol::models::ContentItem; // 导入内容项模型
    use codex_protocol::models::ReasoningItemReasoningSummary; // 导入推理摘要模型
    use codex_protocol::models::ResponseItem; // 导入响应项模型

    /// 创建用户消息的测试工具函数
    fn user_msg(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,                 // 消息ID为空
            role: "user".to_string(), // 角色设为用户
            content: vec![ContentItem::OutputText {
                // 内容为文本输出
                text: text.to_string(), // 消息文本内容
            }],
        }
    }

    /// 创建助手消息的测试工具函数
    fn assistant_msg(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,                      // 消息ID为空
            role: "assistant".to_string(), // 角色设为助手
            content: vec![ContentItem::OutputText {
                // 内容为文本输出
                text: text.to_string(), // 消息文本内容
            }],
        }
    }

    #[test]
    /// 测试截断功能只从最后一个用户消息开始删除
    /// 验证截断逻辑正确识别用户消息并从指定位置开始删除所有后续内容
    fn drops_from_last_user_only() {
        let items = vec![
            user_msg("u1"),      // 第一条用户消息
            assistant_msg("a1"), // 第一条助手消息
            assistant_msg("a2"), // 第二条助手消息
            user_msg("u2"),      // 第二条用户消息（最后一条用户消息）
            assistant_msg("a3"), // 第三条助手消息
            ResponseItem::Reasoning {
                // 推理响应项
                id: "r1".to_string(),
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "s".to_string(),
                }],
                content: None,
                encrypted_content: None,
            },
            ResponseItem::FunctionCall {
                // 函数调用响应项
                id: None,
                name: "tool".to_string(),
                arguments: "{}".to_string(),
                call_id: "c1".to_string(),
            },
            assistant_msg("a4"), // 第四条助手消息
        ];

        // 删除最后1条用户消息，应该从"u2"开始删除到末尾
        let truncated = truncate_after_dropping_last_messages(items.clone(), 1);
        assert_eq!(
            truncated,
            vec![items[0].clone(), items[1].clone(), items[2].clone()] // 应该只保留前3项
        );

        // 删除最后2条用户消息，应该全部删除（因为只有2条用户消息）
        let truncated2 = truncate_after_dropping_last_messages(items, 2);
        assert!(truncated2.is_empty()); // 结果应该为空
    }
}
