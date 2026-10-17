//! Storage-independent durable conversation contract.

use futures_util::future::BoxFuture;
use mirror_protocol::chat_record;
use mirror_protocol::AgentMessage;
use thiserror::Error;

/// Durable conversation storage used by an agent service.
///
/// A store has exactly one owner, the agent runtime that holds the
/// conversation, so operations take `&mut self` and the trait is `Send` rather
/// than `Sync`. Implementations therefore need no interior locking, and a
/// backend that is not `Sync` (a SQLite connection, for example) still yields
/// `Send` futures.
pub trait Store: Send {
    /// True only when this native store inserted this conversation, never inferred
    /// from its transcript length. Disposable/custom stores default to false.
    fn created_session(&self, _session_id: &str) -> bool {
        false
    }

    fn claude_session<'a>(
        &'a mut self,
        _session_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<crate::providers::claude::ClaudeSessionBinding>, StoreError>>
    {
        Box::pin(async { Ok(None) })
    }

    /// Successful reconnect receipts survive worker/transport restart. A receipt
    /// and its binding update must be committed in the same transaction.
    fn claude_reconnect_receipts<'a>(
        &'a mut self,
        _session_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<(String, mirror_protocol::ReconnectClaude)>, StoreError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn compare_set_claude_session<'a>(
        &'a mut self,
        _session_id: &'a str,
        _expected: Option<&'a crate::providers::claude::ClaudeSessionBinding>,
        _binding: &'a crate::providers::claude::ClaudeSessionBinding,
        _receipt: Option<(&'a str, &'a mirror_protocol::ReconnectClaude)>,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async {
            Err(StoreError::Persistence(
                "conversation store does not support Claude binding".into(),
            ))
        })
    }

    /// Load the current window, or the full transcript when explicitly requested.
    fn load<'a>(
        &'a mut self,
        session_id: &'a str,
        full_history: bool,
    ) -> BoxFuture<'a, Result<Vec<AgentMessage>, StoreError>>;

    /// Read a transcript page by row offset, together with its total record count.
    fn messages_page<'a>(
        &'a mut self,
        session_id: &'a str,
        offset: u64,
        limit: u32,
    ) -> BoxFuture<'a, Result<(Vec<AgentMessage>, u64), StoreError>> {
        Box::pin(async move {
            let entries = self.load(session_id, true).await?;
            let total = entries.len() as u64;
            let entries = entries
                .into_iter()
                .skip(usize::try_from(offset).unwrap_or(usize::MAX))
                .take(limit as usize)
                .collect();
            Ok((entries, total))
        })
    }

    fn append<'a>(
        &'a mut self,
        session_id: &'a str,
        head: u64,
        entries: &'a mut [AgentMessage],
    ) -> BoxFuture<'a, Result<(), StoreError>>;
}

#[must_use]
pub fn head(entries: &[AgentMessage]) -> u64 {
    entries.last().and_then(|entry| entry.sequence).unwrap_or(0)
}

pub struct ForkPoint(i64);

impl ForkPoint {
    /// Branch after the current head, including every committed entry.
    pub fn after(head: u64) -> Option<Self> {
        i64::try_from(head).ok()?.checked_add(1).map(Self)
    }

    pub fn parse(entry: &AgentMessage) -> Option<Self> {
        if !matches!(entry.chat_message(), Some(chat_record::Message::User(_))) {
            return None;
        }
        i64::try_from(entry.sequence?)
            .ok()
            .filter(|sequence| *sequence > 0)
            .map(Self)
    }
}

#[derive(Debug, Clone)]
pub struct Lineage<Id> {
    ancestors: Vec<(Id, i64)>,
    session: Id,
}

impl<Id: Eq> Lineage<Id> {
    pub fn parse(
        session: Id,
        mut lineage: Vec<Id>,
        mut fork_points: Vec<i64>,
    ) -> Result<Self, &'static str> {
        if lineage.len() != fork_points.len()
            || lineage.pop().as_ref() != Some(&session)
            || fork_points.pop() != Some(i64::MAX)
        {
            return Err("invalid session lineage");
        }
        if fork_points
            .iter()
            .any(|point| *point < 0 || *point == i64::MAX)
            || fork_points.windows(2).any(|pair| pair[0] > pair[1])
            || lineage.iter().enumerate().any(|(index, ancestor)| {
                ancestor == &session || lineage[..index].contains(ancestor)
            })
        {
            return Err("invalid session ancestors");
        }
        Ok(Self {
            ancestors: lineage.into_iter().zip(fork_points).collect(),
            session,
        })
    }

    pub fn fork(mut self, child: Id, before: ForkPoint) -> Result<Self, &'static str> {
        if self.session == child
            || self
                .ancestors
                .iter()
                .any(|(ancestor, _)| ancestor == &child)
        {
            return Err("fork child already belongs to the lineage");
        }
        let cut = before.0 - 1;
        for (_, point) in &mut self.ancestors {
            *point = (*point).min(cut);
        }
        self.ancestors.push((self.session, cut));
        Ok(Self {
            ancestors: self.ancestors,
            session: child,
        })
    }

    pub fn into_columns(self) -> (Vec<Id>, Vec<i64>) {
        self.ancestors
            .into_iter()
            .chain(std::iter::once((self.session, i64::MAX)))
            .unzip()
    }
}

/// A durable conversation operation failure.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("entry {entry_id:?} is not a user-message rewind boundary")]
    InvalidForkTarget { entry_id: String },
    /// The writer no longer has execution authority for this conversation.
    #[error("session execution lease is no longer held")]
    Fenced,
    #[error("conversation state changed while this agent was running")]
    Stale,
    #[error("session {session_id:?} does not exist")]
    SessionNotFound { session_id: String },
    #[error("conversation {session_id:?} is open in another Mirror process (pid {holder})")]
    OpenElsewhere { session_id: String, holder: String },
    #[error("entry {entry_id:?} is not in session {session_id:?}")]
    EntryNotFound {
        session_id: String,
        entry_id: String,
    },
    #[error("entry {entry_id:?} already exists in session {session_id:?}")]
    MessageIdConflict {
        session_id: String,
        entry_id: String,
    },
    #[error("session {session_id:?} has forks that read its history")]
    SessionHasForks { session_id: String },
    #[error("persistence failed: {0}")]
    Persistence(String),
}
