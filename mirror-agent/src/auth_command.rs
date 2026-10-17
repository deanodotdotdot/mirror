//! Private pre-worker JSONL transport. See docs/claude-integration/AUTH-PROTOCOL.md.
//! Never route these pipes through ordinary worker logging.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use mirror_agent::providers::claude::auth::{
    ClaudeAuthManager, ClaudeCallbackInput, ClaudeLoginEvent, CommandError, Phase,
};
use mirror_agent::providers::claude::{
    ClaudeError, ClaudeErrorKind, ClaudeIdentityBinding, CLAUDE_PROVIDER,
};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::unix::AsyncFd;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

const MAX_INPUT: usize = 16 * 1024;
const MAX_OUTPUT: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Login,
    Status,
    Logout,
    Models,
    ModelContext,
    Usage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyParams {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityParams {
    expected_identity: Option<ClaudeIdentityBinding>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextParams {
    expected_identity: ClaudeIdentityBinding,
    model: String,
    explicit_context_window: Option<u64>,
}
enum Params {
    Empty,
    Identity(IdentityParams),
    Context(ContextParams),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: u32,
    request_id: Uuid,
    #[serde(rename = "type")]
    kind: String,
    provider: String,
    action: Action,
    params: Value,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Control {
    Callback {
        schema_version: u32,
        request_id: Uuid,
        value: String,
    },
    Cancel {
        schema_version: u32,
        request_id: Uuid,
    },
}

struct Pipe {
    file: AsyncFd<File>,
    buffered: Vec<u8>,
}
impl Pipe {
    fn new(fd: i32) -> std::io::Result<Self> {
        // SAFETY: stdin/stdout remain owned by the process; clone owns a new fd.
        let file = File::from(unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?);
        // This internal protocol requires pipes, never a controlling terminal.
        use std::os::unix::fs::FileTypeExt;
        if !file.metadata()?.file_type().is_fifo() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let flags = fcntl(file.as_raw_fd(), FcntlArg::F_GETFL).map_err(std::io::Error::from)?;
        fcntl(
            file.as_raw_fd(),
            FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
        )
        .map_err(std::io::Error::from)?;
        Ok(Self {
            file: AsyncFd::new(file)?,
            buffered: Vec::new(),
        })
    }
    async fn line(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(end) = self.buffered.iter().position(|byte| *byte == b'\n') {
                if end > MAX_INPUT {
                    return Err(std::io::ErrorKind::InvalidData.into());
                }
                return Ok(Some(self.buffered.drain(..=end).collect()));
            }
            if self.buffered.len() > MAX_INPUT {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            let mut ready = self.file.readable().await?;
            let mut bytes = [0; 4096];
            match ready.try_io(|inner| (&mut inner.get_ref()).read(&mut bytes)) {
                Ok(Ok(0)) if self.buffered.is_empty() => return Ok(None),
                Ok(Ok(0)) => return Err(std::io::ErrorKind::InvalidData.into()),
                Ok(Ok(size)) => self.buffered.extend_from_slice(&bytes[..size]),
                Ok(Err(error)) => return Err(error),
                Err(_) => (),
            }
        }
    }
    async fn write(&self, value: &Value) -> std::io::Result<()> {
        let mut bytes = serde_json::to_vec(value).map_err(|_| std::io::ErrorKind::InvalidData)?;
        if bytes.len() > MAX_OUTPUT {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        bytes.push(b'\n');
        tokio::time::timeout(Duration::from_secs(1), async {
            let mut position = 0;
            while position < bytes.len() {
                let mut ready = self.file.writable().await?;
                match ready.try_io(|inner| (&mut inner.get_ref()).write(&bytes[position..])) {
                    Ok(Ok(0)) => return Err(std::io::ErrorKind::WriteZero.into()),
                    Ok(Ok(size)) => position += size,
                    Ok(Err(error)) => return Err(error),
                    Err(_) => (),
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| std::io::ErrorKind::TimedOut)?
    }
}

async fn cancelled(mut receiver: watch::Receiver<bool>) {
    loop {
        if *receiver.borrow() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}
fn invalid() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::InvalidConfiguration)
}
fn base(request: Option<&Request>, kind: &str) -> Value {
    json!({"schema_version":1,"request_id":request.map(|r|r.request_id),"provider":CLAUDE_PROVIDER,"compatibility_profile":mirror_agent::providers::claude::profile::PROFILE_ID,"action":request.map(|r|r.action),"type":kind})
}
async fn failure(output: &Pipe, request: Option<&Request>, error: &CommandError) -> ExitCode {
    let mut value = base(request, "error");
    value["error"] = serde_json::to_value(&error.error).expect("safe shared error");
    value["phase"] = serde_json::to_value(error.phase).expect("fixed phase");
    value["commit_outcome"] =
        serde_json::to_value(error.commit_outcome).expect("fixed commit outcome");
    let _ = output.write(&value).await;
    if matches!(
        error.error.kind,
        ClaudeErrorKind::LoginCancelled | ClaudeErrorKind::RequestCancelled
    ) {
        ExitCode::from(130)
    } else {
        ExitCode::FAILURE
    }
}
fn command_error(error: ClaudeError, phase: Phase) -> CommandError {
    CommandError {
        error,
        phase,
        commit_outcome: None,
    }
}

pub async fn run() -> ExitCode {
    let Ok(output) = Pipe::new(1) else {
        return ExitCode::FAILURE;
    };
    let Ok(mut input) = Pipe::new(0) else {
        return failure(&output, None, &command_error(invalid(), Phase::Protocol)).await;
    };
    if std::env::args().skip(2).collect::<Vec<_>>() != ["--stdio"] {
        return failure(&output, None, &command_error(invalid(), Phase::Protocol)).await;
    }
    let signals = (
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()),
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()),
    );
    let (Ok(mut interrupt), Ok(mut terminate)) = signals else {
        return failure(&output, None, &command_error(invalid(), Phase::Protocol)).await;
    };
    let first = tokio::select! {
        _ = interrupt.recv() => Err(ClaudeError::new(ClaudeErrorKind::RequestCancelled)),
        _ = terminate.recv() => Err(ClaudeError::new(ClaudeErrorKind::RequestCancelled)),
        result = tokio::time::timeout(Duration::from_secs(5), input.line()) => match result {
            Ok(Ok(Some(line))) => Ok(line),
            Ok(Ok(None)) => Err(ClaudeError::new(ClaudeErrorKind::RequestCancelled)),
            _ => Err(invalid()),
        },
    };
    let first = match first {
        Ok(first) => first,
        Err(error) => return failure(&output, None, &command_error(error, Phase::Protocol)).await,
    };
    let request: Request = match serde_json::from_slice(&first) {
        Ok(request) => request,
        Err(_) => return failure(&output, None, &command_error(invalid(), Phase::Protocol)).await,
    };
    if request.schema_version != 1
        || request.request_id.is_nil()
        || request.kind != "request"
        || request.provider != CLAUDE_PROVIDER
    {
        return failure(
            &output,
            Some(&request),
            &command_error(invalid(), Phase::Protocol),
        )
        .await;
    }
    // Validate the complete action-specific shape before any filesystem or
    // authority work. The raw private request is never logged or echoed.
    let params = match request.action {
        Action::Login | Action::Status | Action::Logout => {
            serde_json::from_value::<EmptyParams>(request.params.clone()).map(|_| Params::Empty)
        }
        Action::Models | Action::Usage => {
            serde_json::from_value::<IdentityParams>(request.params.clone()).map(Params::Identity)
        }
        Action::ModelContext => {
            serde_json::from_value::<ContextParams>(request.params.clone()).map(Params::Context)
        }
    };
    let params = match params {
        Ok(params) => params,
        Err(_) => {
            return failure(
                &output,
                Some(&request),
                &command_error(invalid(), Phase::Protocol),
            )
            .await
        }
    };
    let manager = match ClaudeAuthManager::new() {
        Ok(manager) => Arc::new(manager),
        Err(error) => {
            return failure(
                &output,
                Some(&request),
                &command_error(error, Phase::Preparing),
            )
            .await
        }
    };
    let (cancel, receiver) = watch::channel(false);
    let (callbacks, callback_rx) = mpsc::channel(4);
    let (events, mut event_rx) = mpsc::channel(32);
    let operation = async {
        match (request.action, params) {
            (Action::Login, Params::Empty) => manager
                .login(callback_rx, events, Box::pin(cancelled(receiver)))
                .await
                .map(|status| json!({"status":status})),
            (Action::Status, Params::Empty) => manager
                .status(Box::pin(cancelled(receiver)))
                .await
                .map(|status| json!({"status":status}))
                .map_err(|error| command_error(error, Phase::Preparing)),
            (Action::Logout, Params::Empty) => manager
                .logout(Box::pin(cancelled(receiver)))
                .await
                .map(|status| json!({"status":status})),
            (action, params) => {
                let service =
                    mirror_agent::providers::claude::metadata::ClaudeMetadataService::new(
                        manager.clone(),
                    )
                    .map_err(|error| command_error(error, Phase::Preparing))?;
                let result = match (action, params) {
                    (Action::Models, Params::Identity(params)) => service
                        .models(
                            params.expected_identity.as_ref(),
                            None,
                            Box::pin(cancelled(receiver)),
                        )
                        .await
                        .map(|models| json!({"models":models})),
                    (Action::Usage, Params::Identity(params)) => service
                        .usage(
                            params.expected_identity.as_ref(),
                            None,
                            Box::pin(cancelled(receiver)),
                        )
                        .await
                        .map(|usage| json!({"usage":usage})),
                    (Action::ModelContext, Params::Context(params)) => service
                        .model_context(
                            &params.expected_identity,
                            &params.model,
                            params.explicit_context_window,
                            None,
                            Box::pin(cancelled(receiver)),
                        )
                        .await
                        .map(|model_context| json!({"model_context":model_context})),
                    _ => Err(invalid()),
                };
                result.map_err(|error| command_error(error, Phase::Preparing))
            }
        }
    };
    tokio::pin!(operation);
    let mut reading = true;
    loop {
        tokio::select! { biased;
            // Observe already-ready controls before starting/resuming work.
            // Polling the operation first could commit an unstarted logout
            // even though cancel or EOF was already buffered after its request.
            _ = interrupt.recv() => { reading = false; cancel.send_replace(true); },
            _ = terminate.recv() => { reading = false; cancel.send_replace(true); },
            line = input.line(), if reading => {
                match line {
                    Ok(None) => { reading = false; cancel.send_replace(true); },
                    Ok(Some(line)) => {
                        let valid = match serde_json::from_slice::<Control>(&line) {
                            Ok(Control::Cancel { schema_version:1, request_id }) if request_id == request.request_id => { reading = false; cancel.send_replace(true); true },
                            Ok(Control::Callback { schema_version:1, request_id, value }) if request_id == request.request_id && matches!(request.action, Action::Login) => {
                                match ClaudeCallbackInput::new(value) { Ok(value) => { let _ = callbacks.try_send(value); true }, Err(_) => false }
                            },
                            _ => false,
                        };
                        if !valid { return failure(&output, Some(&request), &command_error(invalid(), Phase::Protocol)).await; }
                    },
                    Err(_) => return failure(&output, Some(&request), &command_error(invalid(), Phase::Protocol)).await,
                }
            },
            // A commit returns its terminal result within this same poll.
            // Once selected, output delivery never polls cancellation again.
            result = &mut operation => return match result {
                Ok(result) => completed(&output, &request, result).await,
                Err(error) => failure(&output, Some(&request), &error).await,
            },
            event = event_rx.recv(), if matches!(request.action, Action::Login) => {
                if let Some(event) = event {
                    let value = match event {
                        ClaudeLoginEvent::Progress { phase, callback_rejected } => {
                            let mut value = base(Some(&request), "progress");
                            value["phase"] = serde_json::to_value(phase).unwrap();
                            value["notice"] = if callback_rejected { json!("callback_rejected") } else { Value::Null };
                            value
                        },
                        ClaudeLoginEvent::AuthorizationUrl { authorization_url, expires_at } => {
                            let mut value = base(Some(&request), "authorization_url");
                            value["authorization_url"] = json!(authorization_url.expose_for_login_ui());
                            value["expires_at"] = json!(prost_types::Timestamp::from(expires_at).to_string());
                            value
                        }
                    };
                    if output.write(&value).await.is_err() { cancel.send_replace(true); }
                }
            },
        }
    }
}
async fn completed(output: &Pipe, request: &Request, result: Value) -> ExitCode {
    let mut value = base(Some(request), "completed");
    value["result"] = result;
    if output.write(&value).await.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
