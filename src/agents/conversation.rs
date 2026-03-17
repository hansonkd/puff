use crate::agents::llm::{Message, MessageContent, Role};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Conversation {
    pub id: String,
    pub agent_name: String,
    pub messages: Vec<Message>,
    pub token_count: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Conversation {
    pub fn new(agent_name: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            agent_name: agent_name.to_string(),
            messages: Vec::new(),
            token_count: 0,
            created_at: chrono::Utc::now(),
        }
    }

    pub fn with_id(mut self, id: String) -> Self {
        self.id = id;
        self
    }

    pub fn add_user_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::User,
            content: MessageContent::Text(content.to_string()),
            tool_call_id: None,
        });
    }

    pub fn add_assistant_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::Assistant,
            content: MessageContent::Text(content.to_string()),
            tool_call_id: None,
        });
    }

    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
    }
}
