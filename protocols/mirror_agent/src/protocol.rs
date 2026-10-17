//! Generated Mirror agent protocol bindings.

tonic::include_proto!("mirror.agent.v1");

/// Protocol descriptors for native JSON trace artifacts.
pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("descriptor");

impl Compaction {
    pub fn to_llm_messages(&self) -> Vec<chat_record::Message> {
        let mut messages = Vec::new();
        if let Some(summary) = self
            .summary
            .as_deref()
            .filter(|summary| !summary.is_empty())
        {
            messages.push(chat_record::Message::User(UserMessage {
                checkpoint: None,
                content: format!(
                    "The conversation so far was compacted into this summary. Continue from where it leaves off instead of starting over:\n<summary>\n{}\n</summary>",
                    summary
                ),
            }));
        }
        messages.extend(self.recent.iter().flat_map(AgentMessage::to_llm_messages));
        messages
    }
}

impl AgentMessage {
    /// Describe an action's approval without retaining its arguments or provider fields.
    pub fn tool_approval(
        call: &ToolCall,
        outcome: ToolApprovalOutcome,
        source: ApprovalSource,
    ) -> Self {
        Self {
            payload: Some(agent_message::Payload::ToolApproval(ToolApprovalRecord {
                call_id: call.id.clone(),
                tool_name: call
                    .function
                    .as_ref()
                    .map(|function| function.name.clone())
                    .unwrap_or_default(),
                outcome: outcome.into(),
                source: source.into(),
            })),
            ..Self::default()
        }
    }

    /// The chat payload carried by this trace record, when present.
    pub fn chat_message(&self) -> Option<&chat_record::Message> {
        match &self.payload {
            Some(agent_message::Payload::Chat(ChatRecord {
                message: Some(message),
                ..
            })) => Some(message),
            _ => None,
        }
    }

    /// Project this trace record into messages sent to the model.
    pub fn to_llm_messages(&self) -> Vec<chat_record::Message> {
        let Some(payload) = &self.payload else {
            return Vec::new();
        };
        match payload {
            agent_message::Payload::Chat(chat) => {
                // Unsigned thinking and incomplete tool fragments remain visible
                // in the transcript but cannot form an empty assistant request.
                if !chat.success && chat.truncated {
                    if let Some(chat_record::Message::Assistant(assistant)) = &chat.message {
                        if assistant.content.as_deref().is_none_or(str::is_empty)
                            && assistant.refusal.as_deref().is_none_or(str::is_empty)
                            && assistant.tool_calls.is_empty()
                            && assistant.reasoning_details.is_empty()
                        {
                            return Vec::new();
                        }
                    }
                }
                chat.message.clone().into_iter().collect()
            }
            agent_message::Payload::ContextWindow(window) => {
                let mut messages = Vec::new();
                if !window.system_prompt.trim().is_empty() {
                    messages.push(chat_record::Message::System(SystemMessage {
                        content: window.system_prompt.clone(),
                    }));
                }
                if let Some(compaction) = &window.compaction {
                    messages.extend(compaction.to_llm_messages());
                }
                messages
            }
            agent_message::Payload::TaskResult(result) => {
                let status = if result.success {
                    "succeeded"
                } else {
                    "failed"
                };
                vec![chat_record::Message::User(UserMessage {
                checkpoint: None,
                    content: format!(
                        "<task_result id=\"{}\" tool=\"{}\" status=\"{status}\">\n{}\n</task_result>",
                        result.task_id, result.tool_name, result.content
                    ),
                })]
            }
            agent_message::Payload::Extension(extension) => extension
                .model_message
                .clone()
                .map(chat_record::Message::User)
                .into_iter()
                .collect(),
            agent_message::Payload::Log(_) | agent_message::Payload::ToolApproval(_) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn reconnect_wire_tags_are_additive_and_old_ack_still_decodes() {
        let descriptors = prost_types::FileDescriptorSet::decode(FILE_DESCRIPTOR_SET).unwrap();
        let field = |message: &str, name: &str| {
            descriptors
                .file
                .iter()
                .flat_map(|file| &file.message_type)
                .find(|item| item.name.as_deref() == Some(message))
                .unwrap()
                .field
                .iter()
                .find(|field| field.name.as_deref() == Some(name))
                .unwrap()
                .number
                .unwrap()
        };
        for (message, name, tag) in [
            ("ClientMessage", "reconnect_claude", 13),
            ("ReconnectClaude", "expected_binding", 1),
            ("ReconnectClaude", "target_identity", 2),
            ("SessionMessage", "claude_session_state", 12),
            ("ClaudeSessionState", "conversation_id", 1),
            ("ClaudeSessionState", "binding", 2),
            ("ClaudeSessionState", "message_id", 3),
            ("InputNack", "claude_error", 4),
        ] {
            assert_eq!(field(message, name), tag);
        }
        assert_eq!(InputNackReason::ConversationBusy as i32, 5);
        let old = SessionMessage::decode(&b"\x42\x03\x0a\x01x"[..]).unwrap();
        assert!(
            matches!(old.payload, Some(session_message::Payload::InputAck(ack)) if ack.message_id == "x")
        );
        let unbound = ClaudeSessionState::decode(&b"\x0a\x01s"[..]).unwrap();
        assert!(unbound.binding.is_none());
        assert!(unbound.message_id.is_none());
    }

    #[test]
    fn incomplete_fragments_stay_in_history_without_empty_or_executable_replay() {
        let mut entry = AgentMessage {
            payload: Some(agent_message::Payload::Chat(ChatRecord {
                success: false,
                truncated: true,
                display: Some("Incomplete tool call: {".into()),
                message: Some(chat_record::Message::Assistant(AssistantMessage {
                    reasoning_content: Some("unfinished thinking".into()),
                    ..Default::default()
                })),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert!(entry.to_llm_messages().is_empty());
        assert!(entry.chat_message().is_some());
        let Some(agent_message::Payload::Chat(chat)) = &mut entry.payload else {
            unreachable!()
        };
        let Some(chat_record::Message::Assistant(assistant)) = &mut chat.message else {
            unreachable!()
        };
        assistant.content = Some("visible partial answer".into());
        let projected = entry.to_llm_messages();
        assert_eq!(projected.len(), 1);
        let chat_record::Message::Assistant(assistant) = &projected[0] else {
            unreachable!()
        };
        assert!(assistant.tool_calls.is_empty());
        assert!(assistant.reasoning_details.is_empty());
    }
}
