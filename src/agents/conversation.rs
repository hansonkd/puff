//! Conversation message history management.

use crate::agents::llm::{Message, MessageContent, Role};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Conversation {
    pub id: String,
    pub agent_name: String,
    pub messages: Vec<Message>,
    archived_context: String,
    pub token_count: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl Conversation {
    /// Create a new conversation with a generated UUID.
    pub fn new(agent_name: &str) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            agent_name: agent_name.to_string(),
            messages: Vec::new(),
            archived_context: String::new(),
            token_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_id(mut self, id: String) -> Self {
        self.id = id;
        self
    }

    /// Append a user message to the conversation.
    pub fn add_user_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::User,
            content: MessageContent::Text(content.to_string()),
            tool_call_id: None,
        });
        self.touch();
    }

    /// Append an assistant message to the conversation.
    pub fn add_assistant_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::Assistant,
            content: MessageContent::Text(content.to_string()),
            tool_call_id: None,
        });
        self.touch();
    }

    /// Append an arbitrary message to the conversation.
    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
        self.touch();
    }

    pub fn archived_context(&self) -> Option<&str> {
        if self.archived_context.is_empty() {
            None
        } else {
            Some(self.archived_context.as_str())
        }
    }

    pub fn compact(&mut self, keep_recent_messages: usize, max_archived_chars: usize) {
        if keep_recent_messages == 0 || self.messages.len() <= keep_recent_messages {
            return;
        }

        let archived_count = self.messages.len() - keep_recent_messages;
        let drained: Vec<Message> = self.messages.drain(..archived_count).collect();

        let mut archived = String::new();
        for message in drained {
            append_compacted_message(&mut archived, &message);
        }

        if archived.is_empty() {
            return;
        }

        if !self.archived_context.is_empty() {
            self.archived_context.push('\n');
        }
        self.archived_context.push_str(&archived);

        if self.archived_context.len() > max_archived_chars {
            let keep_from = find_char_boundary(
                &self.archived_context,
                self.archived_context.len() - max_archived_chars,
            );
            self.archived_context = format!(
                "[Earlier context truncated]\n{}",
                &self.archived_context[keep_from..]
            );
        }

        self.touch();
    }

    fn touch(&mut self) {
        self.updated_at = chrono::Utc::now();
    }
}

fn append_compacted_message(buffer: &mut String, message: &Message) {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };

    match &message.content {
        MessageContent::Text(text) => {
            buffer.push_str(role);
            buffer.push_str(": ");
            buffer.push_str(text);
            buffer.push('\n');
        }
        MessageContent::Blocks(blocks) => {
            for block in blocks {
                buffer.push_str(role);
                buffer.push_str(": ");
                match block {
                    crate::agents::llm::ContentBlock::Text { text } => {
                        buffer.push_str(text);
                    }
                    crate::agents::llm::ContentBlock::ToolUse { name, input, .. } => {
                        buffer.push_str("[tool_use ");
                        buffer.push_str(name);
                        buffer.push_str("] ");
                        buffer.push_str(&input.to_string());
                    }
                    crate::agents::llm::ContentBlock::ToolResult {
                        content, is_error, ..
                    } => {
                        if is_error.unwrap_or(false) {
                            buffer.push_str("[tool_result error] ");
                        } else {
                            buffer.push_str("[tool_result] ");
                        }
                        buffer.push_str(content);
                    }
                }
                buffer.push('\n');
            }
        }
    }
}

fn find_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_moves_old_messages_into_archive() {
        let mut conversation = Conversation::new("tester");
        conversation.add_user_message("first");
        conversation.add_assistant_message("second");
        conversation.add_user_message("third");

        conversation.compact(2, 1024);

        assert_eq!(conversation.messages.len(), 2);
        assert_eq!(conversation.archived_context(), Some("user: first\n"));
    }

    #[test]
    fn compact_truncates_archived_context() {
        let mut conversation = Conversation::new("tester");
        conversation.add_user_message("alpha");
        conversation.add_assistant_message("beta");
        conversation.add_user_message("gamma");

        conversation.compact(1, 12);

        let archived = conversation.archived_context().unwrap();
        assert!(archived.starts_with("[Earlier context truncated]"));
        assert!(archived.contains("beta"));
    }
}
