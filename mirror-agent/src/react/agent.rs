use std::time::{Duration, Instant};

use crate::compaction::active_messages;
use crate::compaction::boundary;
use crate::compaction::completion_budget;
use crate::compaction::estimate_tools;
use crate::compaction::prompt_size;
use crate::compaction::Compactor;
use crate::compaction::CompactorConfig;
use crate::providers::Completion;
use crate::providers::CompletionRequest;
use crate::providers::{model_context, ModelProvider};
use crate::tools::ToolOutput;
use crate::AgentError;
use futures_util::StreamExt;
use mirror_protocol::agent_message;
use mirror_protocol::chat_record;
use mirror_protocol::AgentMessage;
use mirror_protocol::ApprovalSource;
use mirror_protocol::AssistantMessage;
use mirror_protocol::ChatRecord;
use mirror_protocol::Compaction;
use mirror_protocol::ContextWindowRecord;
use mirror_protocol::InferenceConfig;
use mirror_protocol::LogRecord;
use mirror_protocol::ToolApprovalOutcome;
use mirror_protocol::ToolCall;
use mirror_protocol::ToolMessage;
use uuid::Uuid;

use crate::providers::Llm;
use crate::providers::LlmError;
use crate::providers::StreamEvent;
use crate::system_prompt::build_system_prompt;
use crate::system_prompt::SystemPromptConfig;
use crate::tools::ToolExecutor;
use crate::tools::ToolExecutorError;
use crate::AgentEvent;
use crate::ExecutionContextError;
use mirror_protocol::SystemPromptReport;

use super::ReactContext;
use super::ReactContextError;

pub struct ReactAgent<Model> {
    model: Model,
    inference_provider: ModelProvider,
    inference_config: Option<InferenceConfig>,
    context_window: Option<usize>,
    context_notice: Option<String>,
    last_context_notice: Option<String>,
    compaction_model: Option<Model>,
    system_prompt: SystemPromptConfig,
    config: ReactAgentConfig,
}

impl<Model> ReactAgent<Model>
where
    Model: Llm,
{
    pub fn new(
        model: Model,
        inference_provider: ModelProvider,
        compaction_model: Option<Model>,
        _tool_executor: &dyn ToolExecutor,
        system_prompt: SystemPromptConfig,
        config: ReactAgentConfig,
    ) -> Result<Self, ReactAgentError> {
        let agent = Self {
            model,
            inference_provider,
            inference_config: None,
            context_window: None,
            context_notice: None,
            last_context_notice: None,
            compaction_model,
            system_prompt,
            config,
        };
        Ok(agent)
    }

    pub(crate) fn settings_provider(&self) -> ModelProvider {
        self.inference_provider.clone()
    }

    pub(crate) fn attach_claude_session(
        &mut self,
        session: crate::providers::claude::ClaudeSession,
    ) {
        self.inference_provider.set_claude_session(session.clone());
        self.model.set_claude_session(session.clone());
        if let Some(model) = &mut self.compaction_model {
            model.set_claude_session(session);
        }
        self.context_window = None;
    }

    pub(crate) fn needs_initial_claude_binding(&self) -> bool {
        self.inference_provider.uses_subscription()
            || self
                .compaction_model
                .as_ref()
                .and_then(Llm::inference_defaults)
                .is_some_and(|inference| inference.api.as_deref() == Some("anthropic-oauth"))
    }

    pub(crate) fn invalidate_claude_context(&mut self) {
        self.context_window = None;
        if let Some(inference) = &mut self.inference_config {
            inference.claude_session = self
                .inference_provider
                .session()
                .binding()
                .as_ref()
                .map(Into::into);
        }
    }

    pub(crate) fn prompt_injection_policy(&self) -> Option<String> {
        self.system_prompt.prompt_injection_policy.clone()
    }

    fn update_inference(&mut self, config: InferenceConfig) {
        let changed_model = match (
            self.inference_provider
                .config_for(self.inference_config.as_ref()),
            self.inference_provider.config_for(Some(&config)),
        ) {
            (Ok(previous), Ok(next)) => {
                previous.model != next.model
                    || previous.api != next.api
                    || previous.base_url != next.base_url
                    || previous.claude_session != next.claude_session
            }
            _ => true,
        };
        if changed_model {
            self.context_window = None;
        }
        self.inference_config = Some(config);
    }

    /// Apply pending inference changes and publish usage without starting a turn.
    pub async fn refresh_inference(
        &mut self,
        context: &mut ReactContext,
        tool_executor: &dyn ToolExecutor,
    ) -> Result<(), ReactAgentError> {
        let Some(inference) = context.execution().inference_config() else {
            return Ok(());
        };
        if self.inference_config.as_ref() == Some(&inference) {
            return Ok(());
        }
        self.update_inference(inference);
        let context_window = self.context_window().await?;
        self.report_context_usage(context, tool_executor, context_window)
            .await
    }

    /// API providers discover once per selection. Claude's metadata service owns
    /// its cache so every use revalidates identity, generation and the guard.
    pub async fn context_window(&mut self) -> Result<usize, ReactAgentError> {
        let config = self
            .inference_provider
            .config_for(self.inference_config.as_ref())?;
        if config.api != crate::providers::ModelApi::AnthropicOAuth {
            if let Some(window) = self.context_window {
                return Ok(window);
            }
        }
        let resolved = model_context(&self.inference_provider, &config).await?;
        self.context_notice = resolved
            .notice
            .filter(|notice| self.last_context_notice.as_ref() != Some(notice));
        self.context_window = Some(resolved.window);
        Ok(resolved.window)
    }

    fn compactor(
        &self,
        context_window: usize,
        system_prompt: &str,
        tool_executor: &dyn ToolExecutor,
    ) -> Result<Compactor, ReactAgentError> {
        Ok(Compactor::new(
            CompactorConfig {
                context_window,
                compaction_threshold: self.config.compaction_threshold,
                max_tokens: self.config.max_tokens,
                tool_result_max_chars: self.config.tool_result_max_chars,
                prompt_injection_policy: self.system_prompt.prompt_injection_policy.clone(),
            },
            &tool_executor.definitions(),
            system_prompt,
        )?)
    }

    /// Check that the system prompt and tools leave room under the compaction line.
    ///
    /// Without compaction the line never limits the prompt, so only the full
    /// window applies, and `completion_budget` enforces that per step.
    fn validate_budget(
        &self,
        context_window: usize,
        system_prompt: &str,
        tool_executor: &dyn ToolExecutor,
    ) -> Result<(), ReactAgentError> {
        if !self.config.disable_compaction {
            self.compactor(context_window, system_prompt, tool_executor)?;
        }
        Ok(())
    }

    async fn snapshot_system_prompt(
        &self,
        tool_executor: &dyn ToolExecutor,
    ) -> Result<(String, SystemPromptReport), ReactAgentError> {
        let config = self.system_prompt.clone();
        let (prompt, mut report) =
            tokio::task::spawn_blocking(move || build_system_prompt(&config))
                .await
                .map_err(|error| AgentError::InvalidInput(error.to_string()))?;
        update_tool_report(&mut report, tool_executor);
        Ok((prompt, report))
    }

    pub async fn initialize(
        &mut self,
        context: &mut ReactContext,
        tool_executor: &dyn ToolExecutor,
    ) -> Result<(), ReactAgentError> {
        if let Some(inference) = context.execution().inference_config() {
            self.update_inference(inference);
        }
        let inference = self.inference_config.clone();
        if context.entries().is_empty() {
            let system_prompt = self.snapshot_system_prompt(tool_executor).await?;
            let window = self.context_window_record(inference, system_prompt, None);
            context.push(AgentEvent::Entries(vec![window])).await?;
            let context_window = self.context_window().await?;
            self.validate_budget(context_window, context.system_prompt()?, tool_executor)?;
            self.report_context_usage(context, tool_executor, context_window)
                .await?;
            return Ok(());
        }
        context.strip_before_active_window()?;
        if let Some(mut entry) = context.entries().first().cloned() {
            if let Some(agent_message::Payload::ContextWindow(window)) = &mut entry.payload {
                if let Some(report) = &mut window.system_prompt_report {
                    update_tool_report(report, tool_executor);
                    context.publish(AgentEvent::Entries(vec![entry]));
                }
            }
        }
        let context_window = self.context_window().await?;
        self.report_context_usage(context, tool_executor, context_window)
            .await
    }

    /// Compact on request using the automatic policy, without a completion turn.
    pub async fn compact_now(
        &mut self,
        context: &mut ReactContext,
        tool_executor: &dyn ToolExecutor,
    ) -> Result<(), ReactAgentError> {
        if self.config.disable_compaction {
            return Err(AgentError::InvalidInput("compaction is disabled".into()).into());
        }
        let window = self.context_window().await?;
        self.publish_context_notice(context).await?;
        self.compact(
            context,
            tool_executor,
            window,
            self.inference_config.as_ref(),
        )
        .await?;
        Ok(())
    }

    pub async fn run(
        &mut self,
        context: &mut ReactContext,
        tool_executor: &dyn ToolExecutor,
    ) -> Result<(), ReactAgentError> {
        let mut exceptions = 0;
        let mut retry_delay = Duration::ZERO;
        let mut incomplete_completions = 0;
        for _iteration in 0..self.config.max_iterations {
            if !retry_delay.is_zero() {
                tokio::time::sleep(retry_delay).await;
            }
            match self.step(context, tool_executor).await {
                Ok(unfinished) => {
                    retry_delay = Duration::ZERO;
                    incomplete_completions = 0;
                    let pending = context.flush_pending();
                    let has_model_input = pending
                        .iter()
                        .any(|entry| !entry.to_llm_messages().is_empty());
                    if !pending.is_empty() {
                        context.push(AgentEvent::Entries(pending)).await?;
                    }
                    if !unfinished && !has_model_input {
                        return Ok(());
                    }
                }
                Err(ReactAgentError::IncompleteCompletion) => {
                    incomplete_completions += 1;
                    if incomplete_completions >= 3 {
                        return Err(ReactAgentError::IncompleteCompletion);
                    }
                    context
                        .push(AgentEvent::Entries(vec![log_message(
                            "model returned no tool call or final answer; retrying at most twice",
                            "warning",
                        )]))
                        .await?;
                }
                Err(error @ ReactAgentError::Model(LlmError::ContextOverflow(_)))
                    if self.config.disable_compaction =>
                {
                    return Err(error);
                }
                Err(error) if error.is_retryable() => {
                    context
                        .push(AgentEvent::Entries(vec![log_message(
                            &error.to_string(),
                            "error",
                        )]))
                        .await?;
                    exceptions += 1;
                    retry_delay = retry_delay.saturating_add(Duration::from_secs(1));
                    if self.config.max_exceptions > 0 && exceptions >= self.config.max_exceptions {
                        context
                            .push(AgentEvent::Entries(vec![assistant_text(&format!(
                                "Reached the maximum of {} exceptions without completing the task.",
                                self.config.max_exceptions
                            ))]))
                            .await?;
                        return Err(error);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        context
            .push(AgentEvent::Entries(vec![assistant_text(&format!(
                "Reached the maximum of {} iterations without completing the task.",
                self.config.max_iterations
            ))]))
            .await?;
        Ok(())
    }

    async fn step(
        &mut self,
        context: &mut ReactContext,
        tool_executor: &dyn ToolExecutor,
    ) -> Result<bool, ReactAgentError> {
        if let Some(inference) = context.execution().inference_config() {
            self.update_inference(inference);
        }
        let context_window = self.context_window().await?;
        let inference = self.inference_config.clone();
        self.report_context_usage(context, tool_executor, context_window)
            .await?;
        let messages = self
            .maybe_compact(context, tool_executor, context_window, inference.as_ref())
            .await?;
        let prompt = self.prompt_tokens(context, tool_executor, context_window)?;
        let budget = self.completion_budget(context_window, prompt)?;
        let (completion, tokens_per_second, budget) = match self
            .complete(context, tool_executor, messages, budget, inference.as_ref())
            .await
        {
            Ok((completion, tokens_per_second)) => (completion, tokens_per_second, budget),
            Err(ReactAgentError::Model(LlmError::ContextOverflow(detail)))
                if !self.config.disable_compaction =>
            {
                context
                    .push(AgentEvent::Entries(vec![log_message(
                        &format!("{detail}; compacting"),
                        "warning",
                    )]))
                    .await?;
                let messages = self
                    .compact(context, tool_executor, context_window, inference.as_ref())
                    .await?;
                let budget = self.completion_budget(
                    context_window,
                    self.prompt_tokens(context, tool_executor, context_window)?,
                )?;
                let (completion, tokens_per_second) = self
                    .complete(context, tool_executor, messages, budget, inference.as_ref())
                    .await?;
                (completion, tokens_per_second, budget)
            }
            Err(error) => return Err(error),
        };
        let calls = completion.message.tool_calls.clone();
        let truncated = completion.truncated;
        let has_answer = completion
            .message
            .content
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
            || completion
                .message
                .refusal
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty());
        context
            .push(AgentEvent::StreamFinal {
                entry: Box::new(assistant_record(completion, inference.clone())),
                tokens_per_second,
            })
            .await?;
        if !calls.is_empty() {
            self.run_tools(context, tool_executor, &calls).await?;
        }
        if !self.config.disable_compaction && truncated && budget < self.config.max_tokens {
            context
                .push(AgentEvent::Entries(vec![log_message(
                    "response was cut short by the completion budget; compacting",
                    "warning",
                )]))
                .await?;
            self.compact(context, tool_executor, context_window, inference.as_ref())
                .await?;
        }
        if calls.is_empty() && (!has_answer || truncated) {
            return Err(ReactAgentError::IncompleteCompletion);
        }
        Ok(!calls.is_empty())
    }

    async fn maybe_compact(
        &self,
        context: &mut ReactContext,
        tool_executor: &dyn ToolExecutor,
        context_window: usize,
        inference: Option<&InferenceConfig>,
    ) -> Result<Vec<chat_record::Message>, ReactAgentError> {
        if self.config.disable_compaction {
            context.system_prompt()?;
            return Ok(active_messages(context.entries()));
        }
        let compactor = self.compactor(context_window, context.system_prompt()?, tool_executor)?;
        if compactor.should_compact(context.entries(), &[]) {
            self.compact(context, tool_executor, context_window, inference)
                .await
        } else {
            Ok(compactor.messages_with(context.entries(), &[]))
        }
    }

    async fn compact(
        &self,
        context: &mut ReactContext,
        tool_executor: &dyn ToolExecutor,
        context_window: usize,
        inference: Option<&InferenceConfig>,
    ) -> Result<Vec<chat_record::Message>, ReactAgentError> {
        let system_prompt = self.snapshot_system_prompt(tool_executor).await?;
        let compactor = self.compactor(context_window, &system_prompt.0, tool_executor)?;
        context.push(AgentEvent::CompactionStarted).await?;
        let (warnings, request) = compactor.prepare_with(context.entries(), &[]);
        if !warnings.is_empty() {
            context.push(AgentEvent::Entries(warnings)).await?;
        }
        let used_inference = if request.is_some() {
            self.compaction_model
                .as_ref()
                .and_then(|model| model.compaction_inference(inference))
                .or_else(|| inference.cloned())
        } else {
            None
        };
        let completion = match request {
            Some(mut request) => {
                request.inference = used_inference.clone();
                if let Some(selected) = &used_inference {
                    if let Some(api) = &selected.api {
                        let billing = if api == "anthropic-messages" {
                            " (API-key billing)"
                        } else {
                            ""
                        };
                        context
                            .push(AgentEvent::Entries(vec![log_message(
                                &format!("Compaction uses {} via {api}{billing}.", selected.model),
                                "info",
                            )]))
                            .await?;
                    }
                }
                Some(self.summarize(context, request).await?)
            }
            None => None,
        };
        let mut compaction = compactor.finish(context.entries(), completion)?;
        compaction.model = used_inference
            .as_ref()
            .filter(|config| !config.model.is_empty())
            .map_or_else(
                || self.compaction_model().model().to_string(),
                |config| config.model.clone(),
            );
        compaction.inference = used_inference;
        let window =
            self.context_window_record(inference.cloned(), system_prompt, Some(compaction));
        context.push(AgentEvent::Entries(vec![window])).await?;
        context.strip_before_active_window()?;
        context
            .push(AgentEvent::ContextUsage {
                tokens: compactor.prompt_tokens(context.entries()),
                window: context_window,
            })
            .await?;
        Ok(compactor.messages_with(context.entries(), &[]))
    }

    async fn complete(
        &self,
        context: &mut ReactContext,
        tool_executor: &dyn ToolExecutor,
        messages: Vec<chat_record::Message>,
        max_tokens: usize,
        inference: Option<&InferenceConfig>,
    ) -> Result<(Completion, Option<f64>), ReactAgentError> {
        let started = Instant::now();
        let events = self.model.stream(CompletionRequest {
            messages,
            tools: tool_executor.definitions(),
            max_tokens: Some(max_tokens.try_into().unwrap_or(u32::MAX)),
            inference: inference.cloned(),
        });
        futures_util::pin_mut!(events);
        let mut final_completion = None;
        let mut partial = PartialResponse::default();
        while let Some(event) = events.next().await {
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    context.push(AgentEvent::StreamAborted).await?;
                    if matches!(error, LlmError::Claude(_)) {
                        partial.persist(context, inference.cloned()).await?;
                    }
                    return Err(error.into());
                }
            };
            partial.observe(&event);
            if final_completion.is_some() && !matches!(event, StreamEvent::Final(_)) {
                context.push(AgentEvent::StreamAborted).await?;
                return Err(ReactAgentError::InvalidStream(
                    "stream produced a partial after its final message".to_string(),
                ));
            }
            match event {
                StreamEvent::Content(content) => {
                    context.push(AgentEvent::StreamContent(content)).await?
                }
                StreamEvent::Reasoning(reasoning) => {
                    context.push(AgentEvent::StreamReasoning(reasoning)).await?
                }
                StreamEvent::Refusal(refusal) => {
                    context.push(AgentEvent::StreamRefusal(refusal)).await?
                }
                StreamEvent::ToolCall(tool_call) => {
                    context.push(AgentEvent::StreamToolCall(tool_call)).await?
                }
                StreamEvent::OutputTokens(completion_tokens) => {
                    context
                        .push(AgentEvent::StreamUsage(mirror_protocol::StreamUsage {
                            completion_tokens,
                            elapsed_seconds: started.elapsed().as_secs_f64(),
                        }))
                        .await?;
                }
                StreamEvent::Notice(notice) => {
                    let mut record = log_message("Omitted incompatible opaque reasoning while preserving visible history and tool results.", "notice");
                    if let Some(agent_message::Payload::Log(log)) = record.payload.as_mut() {
                        log.history_normalization = Some(notice.to_protocol());
                    }
                    context.push(AgentEvent::Entries(vec![record])).await?;
                }
                StreamEvent::Final(completion) => {
                    if final_completion.replace(completion).is_some() {
                        context.push(AgentEvent::StreamAborted).await?;
                        return Err(ReactAgentError::InvalidStream(
                            "stream produced more than one final message".to_string(),
                        ));
                    }
                }
            }
        }
        let Some(completion) = final_completion else {
            context.push(AgentEvent::StreamAborted).await?;
            return Err(ReactAgentError::InvalidStream(
                "stream produced no final message".to_string(),
            ));
        };
        let elapsed = started.elapsed().as_secs_f64();
        let tokens_per_second = completion.usage.as_ref().and_then(|usage| {
            (elapsed > 0.0 && usage.completion_tokens > 0)
                .then_some(usage.completion_tokens as f64 / elapsed)
        });
        Ok((completion, tokens_per_second))
    }

    async fn summarize(
        &self,
        context: &mut ReactContext,
        request: CompletionRequest,
    ) -> Result<Completion, ReactAgentError> {
        let events = self.compaction_model().stream(request);
        futures_util::pin_mut!(events);
        let mut completion = None;
        let mut partial = PartialResponse::default();
        while let Some(event) = events.next().await {
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    if matches!(error, LlmError::Claude(_)) {
                        partial.persist_summary(context).await?;
                    }
                    return Err(error.into());
                }
            };
            partial.observe(&event);
            if completion.is_some() && !matches!(event, StreamEvent::Final(_)) {
                return Err(ReactAgentError::InvalidStream(
                    "summarization stream produced a partial after its final message".to_string(),
                ));
            }
            if let StreamEvent::Final(final_completion) = event {
                if final_completion.truncated {
                    return Err(ReactAgentError::InvalidStream(
                        "summarization stream produced a truncated final message".to_string(),
                    ));
                }
                if completion.replace(final_completion).is_some() {
                    return Err(ReactAgentError::InvalidStream(
                        "summarization stream produced more than one final message".to_string(),
                    ));
                }
            }
        }
        completion.ok_or_else(|| {
            ReactAgentError::InvalidStream(
                "summarization stream produced no final message".to_string(),
            )
        })
    }

    async fn run_tools(
        &self,
        context: &mut ReactContext,
        tool_executor: &dyn ToolExecutor,
        calls: &[ToolCall],
    ) -> Result<(), ReactAgentError> {
        let batch_id = Uuid::new_v4();
        let execution = context.execution().clone();
        let mut allowed = Vec::with_capacity(calls.len());
        for call in calls {
            let needs_approval = call
                .function
                .as_ref()
                .is_some_and(|function| tool_executor.needs_approval(&function.name));
            allowed.push(approve_tool(context, call, needs_approval).await?);
        }
        let outputs = tool_executor
            .execute(batch_id, calls, &allowed, &execution)
            .await?;
        if outputs.len() != calls.len() {
            return Err(ToolExecutorError::ResultCount {
                expected: calls.len(),
                actual: outputs.len(),
            }
            .into());
        }
        let replies = calls
            .iter()
            .zip(outputs)
            .map(|(call, output)| tool_record(call, output))
            .collect();
        context.push(AgentEvent::Entries(replies)).await?;
        Ok(())
    }

    fn compaction_model(&self) -> &Model {
        self.compaction_model.as_ref().unwrap_or(&self.model)
    }

    async fn report_context_usage(
        &mut self,
        context: &mut ReactContext,
        tool_executor: &dyn ToolExecutor,
        context_window: usize,
    ) -> Result<(), ReactAgentError> {
        self.publish_context_notice(context).await?;
        context
            .push(AgentEvent::ContextUsage {
                tokens: self.prompt_tokens(context, tool_executor, context_window)?,
                window: context_window,
            })
            .await?;
        Ok(())
    }

    async fn publish_context_notice(
        &mut self,
        context: &mut ReactContext,
    ) -> Result<(), ReactAgentError> {
        if let Some(notice) = &self.context_notice {
            context
                .push(AgentEvent::Entries(vec![log_message(notice, "info")]))
                .await?;
            self.last_context_notice = self.context_notice.take();
        }
        Ok(())
    }

    fn prompt_tokens(
        &self,
        context: &ReactContext,
        tool_executor: &dyn ToolExecutor,
        context_window: usize,
    ) -> Result<usize, ReactAgentError> {
        if self.config.disable_compaction {
            context.system_prompt()?;
            return Ok(prompt_size(
                context.entries(),
                estimate_tools(&tool_executor.definitions()),
            ));
        }
        Ok(self
            .compactor(context_window, context.system_prompt()?, tool_executor)?
            .prompt_tokens(context.entries()))
    }

    fn completion_budget(
        &self,
        context_window: usize,
        prompt: usize,
    ) -> Result<usize, ReactAgentError> {
        if self.config.disable_compaction {
            let room = context_window.saturating_sub(prompt);
            if room == 0 {
                return Err(LlmError::ContextOverflow(format!(
                    "prompt ({prompt}) exhausts context window ({context_window}); compaction is disabled"
                ))
                .into());
            }
            return Ok(self.config.max_tokens.min(room));
        }
        completion_budget(prompt, context_window, self.config.max_tokens).ok_or_else(|| {
            ReactAgentError::NeedsCompaction(format!(
                "prompt ({prompt}) leaves no room under boundary={}",
                boundary(context_window)
            ))
        })
    }

    fn context_window_record(
        &self,
        inference: Option<InferenceConfig>,
        system_prompt: (String, SystemPromptReport),
        compaction: Option<Compaction>,
    ) -> AgentMessage {
        AgentMessage {
            inference,
            payload: Some(agent_message::Payload::ContextWindow(ContextWindowRecord {
                metadata: None,
                system_prompt: system_prompt.0,
                system_prompt_report: Some(system_prompt.1),
                compaction,
            })),
            ..AgentMessage::default()
        }
    }
}

fn update_tool_report(report: &mut SystemPromptReport, tool_executor: &dyn ToolExecutor) {
    let tools = tool_executor.definitions();
    report.tool_tokens = Some(estimate_tools(&tools) as u64);
    report.tools = tools
        .iter()
        .map(|tool| mirror_protocol::system_prompt_report::Part {
            source: tool.name.clone(),
            estimated_tokens: estimate_tools(std::slice::from_ref(tool)) as u64,
        })
        .collect();
}

#[derive(Debug, Clone, Copy)]
pub struct ReactAgentConfig {
    pub max_iterations: usize,
    /// Maximum retryable exceptions per run; zero disables the limit.
    pub max_exceptions: usize,
    pub compaction_threshold: Option<usize>,
    /// Keep the complete history and fail on context exhaustion instead of compacting.
    pub disable_compaction: bool,
    pub max_tokens: usize,
    pub tool_result_max_chars: usize,
}

impl Default for ReactAgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: usize::MAX,
            max_exceptions: 5,
            compaction_threshold: None,
            disable_compaction: false,
            max_tokens: 64_000,
            tool_result_max_chars: 2000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReactAgentError {
    #[error(transparent)]
    Model(#[from] LlmError),
    #[error(transparent)]
    Context(#[from] ReactContextError),
    #[error(transparent)]
    Execution(#[from] ExecutionContextError),
    #[error(transparent)]
    ToolExecutor(#[from] ToolExecutorError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("model context requires compaction: {0}")]
    NeedsCompaction(String),
    #[error("invalid model stream: {0}")]
    InvalidStream(String),
    #[error("model returned no complete final answer or tool call after bounded retries")]
    IncompleteCompletion,
}

impl ReactAgentError {
    fn is_retryable(&self) -> bool {
        // Transport owns the entire bounded retry budget. Any Claude error
        // reaching this loop is final, including transient recovery guidance.
        if matches!(self, Self::Model(LlmError::Claude(_))) {
            return false;
        }
        if matches!(self, Self::Agent(AgentError::InvalidConfiguration(_))) {
            return false;
        }
        matches!(
            self,
            Self::Model(_) | Self::Agent(_) | Self::NeedsCompaction(_) | Self::InvalidStream(_)
        )
    }
}

fn chat_record_message(message: chat_record::Message) -> AgentMessage {
    AgentMessage {
        payload: Some(agent_message::Payload::Chat(ChatRecord {
            message: Some(message),
            success: true,
            ..ChatRecord::default()
        })),
        ..AgentMessage::default()
    }
}

#[derive(Default)]
struct PartialResponse {
    content: String,
    reasoning: String,
    refusal: String,
    tools: std::collections::BTreeMap<u32, (String, String)>,
}
impl PartialResponse {
    fn observe(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::Content(value) => self.content.push_str(&value.content),
            StreamEvent::Reasoning(value) => self.reasoning.push_str(&value.reasoning),
            StreamEvent::Refusal(value) => self.refusal.push_str(&value.refusal),
            StreamEvent::ToolCall(value) => {
                let tool = self.tools.entry(value.index).or_default();
                tool.0.push_str(&value.name);
                tool.1.push_str(&value.arguments);
            }
            _ => {}
        }
    }
    async fn persist_summary(self, context: &mut ReactContext) -> Result<(), ReactAgentError> {
        if self.content.is_empty()
            && self.reasoning.is_empty()
            && self.refusal.is_empty()
            && self.tools.is_empty()
        {
            return Ok(());
        }
        let mut text = format!(
            "Incomplete compaction summary\n{}{}{}",
            self.reasoning, self.content, self.refusal
        );
        for (name, arguments) in self.tools.values() {
            text.push_str(&format!("\nIncomplete tool call {name}: {arguments}"));
        }
        context
            .push(AgentEvent::Entries(vec![log_message(&text, "error")]))
            .await?;
        Ok(())
    }
    async fn persist(
        self,
        context: &mut ReactContext,
        inference: Option<InferenceConfig>,
    ) -> Result<(), ReactAgentError> {
        if self.content.is_empty()
            && self.reasoning.is_empty()
            && self.refusal.is_empty()
            && self.tools.is_empty()
        {
            return Ok(());
        }
        // No tool call IDs, complete calls or invented signatures enter canonical
        // history. Fragments remain a display-only record beside visible output.
        let mut display = String::from("Incomplete response");
        for (name, arguments) in self.tools.values() {
            display.push_str(&format!("\nIncomplete tool call {name}: {arguments}"));
        }
        let entry = AgentMessage {
            inference,
            payload: Some(agent_message::Payload::Chat(ChatRecord {
                message: Some(chat_record::Message::Assistant(AssistantMessage {
                    content: (!self.content.is_empty()).then_some(self.content),
                    reasoning_content: (!self.reasoning.is_empty()).then_some(self.reasoning),
                    refusal: (!self.refusal.is_empty()).then_some(self.refusal),
                    ..Default::default()
                })),
                success: false,
                truncated: true,
                display: Some(display),
                ..Default::default()
            })),
            ..Default::default()
        };
        context
            .push(AgentEvent::StreamFinal {
                entry: Box::new(entry),
                tokens_per_second: None,
            })
            .await?;
        Ok(())
    }
}

fn assistant_record(completion: Completion, inference: Option<InferenceConfig>) -> AgentMessage {
    AgentMessage {
        inference,
        payload: Some(agent_message::Payload::Chat(ChatRecord {
            message: Some(chat_record::Message::Assistant(completion.message)),
            usage: completion.usage,
            success: true,
            truncated: completion.truncated,
            ..ChatRecord::default()
        })),
        ..AgentMessage::default()
    }
}

fn assistant_text(content: &str) -> AgentMessage {
    chat_record_message(chat_record::Message::Assistant(AssistantMessage {
        content: Some(content.to_string()),
        ..AssistantMessage::default()
    }))
}

fn log_message(text: &str, level: &str) -> AgentMessage {
    AgentMessage {
        payload: Some(agent_message::Payload::Log(LogRecord {
            level: level.to_string(),
            text: text.to_string(),
            ..LogRecord::default()
        })),
        ..AgentMessage::default()
    }
}

fn tool_record(call: &ToolCall, result: ToolOutput) -> AgentMessage {
    AgentMessage {
        metadata: result.metadata(),
        payload: Some(agent_message::Payload::Chat(ChatRecord {
            message: Some(chat_record::Message::Tool(ToolMessage {
                tool_call_id: call.id.clone(),
                content: result.content,
                name: call.function.as_ref().map(|function| function.name.clone()),
            })),
            display: result.display,
            success: result.success,
            ..ChatRecord::default()
        })),
        ..AgentMessage::default()
    }
}

/// Persist permission before acknowledging the decision or allowing execution.
async fn approve_tool(
    context: &mut ReactContext,
    call: &ToolCall,
    needs_approval: bool,
) -> Result<bool, ReactAgentError> {
    let decision = if needs_approval {
        context
            .push(AgentEvent::Entries(vec![AgentMessage::tool_approval(
                call,
                ToolApprovalOutcome::Requested,
                ApprovalSource::Unspecified,
            )]))
            .await?;
        context.execution().approval_decision(call).await?
    } else {
        crate::ApprovalDecision::new(true, ApprovalSource::NotRequired)
    };
    let outcome = if decision.approved {
        ToolApprovalOutcome::Approved
    } else {
        ToolApprovalOutcome::Denied
    };
    context
        .push(AgentEvent::Entries(vec![AgentMessage::tool_approval(
            call,
            outcome,
            decision.source,
        )]))
        .await?;
    Ok(decision.acknowledge())
}

#[cfg(test)]
#[path = "agent_test.rs"]
mod tests;
