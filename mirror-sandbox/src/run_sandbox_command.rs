//! Provider-neutral raw commands over the sandboxd protocol.

use std::future::Future;
use std::time::Duration;

use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tonic::transport::Channel;

use crate::{
    run_command_request, run_command_response, run_command_result::Outcome,
    sandboxd_client::SandboxdClient, CancelRequest, RunCommandRequest, RunCommandResponse,
    RunCommandStart, SandboxStream, SandboxStreamOutcome, SandboxTransportError, SandboxdStream,
    CANCEL_ANSWER_TIMEOUT, COMMAND_CHUNK_BYTES, MAX_MESSAGE_BYTES,
};

const ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
const STDERR_LIMIT: usize = 64 * 1024;
// Each raw command owns a separate connection, and therefore its own ID namespace.
const CALL_ID: &str = "command";

/// Cancellation must be awaited: dropping this future cannot confirm guest exit.
pub async fn run_sandbox_command(
    connection: impl Future<Output = Result<SandboxdStream, SandboxTransportError>> + Send,
    mut request: SandboxStream<'_>,
) -> Result<SandboxStreamOutcome, SandboxTransportError> {
    let deadline = tokio::time::Instant::now()
        .checked_add(request.timeout)
        .ok_or_else(|| SandboxTransportError::Failed("command timeout is too large".into()))?;
    let (closing, _) = watch::channel(false);
    let result = async {
        let connect = async {
            let stream = connection.await?;
            crate::connect(stream.reader, stream.writer, closing.clone()).await
                .map_err(|error| SandboxTransportError::Failed(error.to_string()))
        };
        let client = tokio::select! {
            result = tokio::time::timeout(request.timeout.min(CONNECT_TIMEOUT), connect) => {
                result.map_err(|_| SandboxTransportError::NotReady("sandboxd connection timed out".into()))??
            }
            () = &mut request.cancellation => return Err(SandboxTransportError::Failed("command cancelled before dispatch".into())),
        };
        command_rpc(client, &mut request, deadline).await
    }.await;
    closing.send_replace(true);
    result
}

async fn command_rpc(
    mut client: SandboxdClient<Channel>,
    request: &mut SandboxStream<'_>,
    deadline: tokio::time::Instant,
) -> Result<SandboxStreamOutcome, SandboxTransportError> {
    let start = RunCommandRequest {
        input: Some(run_command_request::Input::Start(RunCommandStart {
            call_id: CALL_ID.into(),
            command: std::mem::take(&mut request.command),
            workdir: request.workdir.take(),
            timeout_millis: u64::try_from(
                deadline
                    .saturating_duration_since(tokio::time::Instant::now())
                    .as_millis(),
            )
            .unwrap_or(u64::MAX),
        })),
    };
    if start.encoded_len() > MAX_MESSAGE_BYTES {
        return Err(SandboxTransportError::Failed(
            "command exceeds the RPC message limit".into(),
        ));
    }
    let (input, receiver) = mpsc::channel(4);
    input.send(start).await.map_err(unknown)?;
    let requests = futures_util::stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|request| (request, receiver))
    });
    let admission = async {
        let mut events = client.run_command(requests).await?.into_inner();
        let first = events.message().await?;
        Ok::<_, tonic::Status>((events, first.and_then(|event| event.event)))
    };
    // Do not let cancellation overtake admission and leave an unregistered ID.
    let (mut events, first) = tokio::time::timeout_at(
        deadline.min(tokio::time::Instant::now() + ADMISSION_TIMEOUT),
        admission,
    )
    .await
    .map_err(unknown)?
    .map_err(unknown)?;
    match first {
        Some(run_command_response::Event::Accepted(true)) => {}
        Some(run_command_response::Event::Completed(result)) => {
            return outcome(result.outcome, Vec::new())
        }
        _ => return Err(unknown("sandboxd did not acknowledge command admission")),
    }
    let mut stderr = Vec::new();
    let completion = {
        let feed = feed_input(input, request.stdin.as_mut());
        let output = receive_output(
            &mut events,
            &mut request.stdout,
            &mut request.stderr,
            &mut stderr,
        );
        let running = async {
            tokio::pin!(output);
            tokio::select! {
                biased;
                result = &mut output => result,
                result = feed => { result?; output.await }
            }
        };
        tokio::select! {
            biased;
            result = running => result,
            () = &mut request.cancellation => Err(SandboxTransportError::Failed("command cancelled".into())),
            () = tokio::time::sleep_until(deadline) => Err(SandboxTransportError::Failed("command deadline exceeded".into())),
        }
    };
    let terminal = match completion {
        Ok(terminal) => terminal,
        Err(error) => {
            let terminal = cancel_and_wait(&mut client, &mut events).await?;
            outcome(terminal, Vec::new())?;
            return Err(SandboxTransportError::Failed(error.to_string()));
        }
    };
    let result = outcome(terminal, stderr)?;
    // Completion is already known; a failed output sink cannot make it unknown.
    let delivery = async {
        request.stdout.shutdown().await?;
        if let Some(stderr) = request.stderr.as_mut() {
            stderr.shutdown().await?;
        }
        Ok::<_, std::io::Error>(())
    };
    tokio::select! {
        result = tokio::time::timeout_at(deadline, delivery) => {
            result.map_err(|_| SandboxTransportError::Failed("command exited but output delivery timed out".into()))?
                .map_err(|error| SandboxTransportError::Failed(format!("command exited but output delivery failed: {error}")))?;
        }
        () = &mut request.cancellation => return Err(SandboxTransportError::Failed("command exited but output delivery was cancelled".into())),
    }
    Ok(result)
}

async fn feed_input(
    input: mpsc::Sender<RunCommandRequest>,
    stdin: Option<&mut std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>>,
) -> Result<(), SandboxTransportError> {
    let Some(stdin) = stdin else {
        return Ok(());
    };
    let mut buffer = vec![0; COMMAND_CHUNK_BYTES];
    loop {
        let count = stdin.read(&mut buffer).await.map_err(unknown)?;
        if count == 0 {
            return Ok(());
        }
        if input
            .send(RunCommandRequest {
                input: Some(run_command_request::Input::Stdin(buffer[..count].to_vec())),
            })
            .await
            .is_err()
        {
            return Ok(());
        }
    }
}

async fn receive_output(
    events: &mut tonic::Streaming<RunCommandResponse>,
    stdout: &mut std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>,
    stderr: &mut Option<std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>>,
    captured: &mut Vec<u8>,
) -> Result<Option<Outcome>, SandboxTransportError> {
    while let Some(event) = events.message().await.map_err(unknown)? {
        match event.event {
            Some(run_command_response::Event::Stdout(bytes)) => {
                stdout.write_all(&bytes).await.map_err(unknown)?
            }
            Some(run_command_response::Event::Stderr(bytes)) => {
                if let Some(stderr) = stderr.as_mut() {
                    stderr.write_all(&bytes).await.map_err(unknown)?;
                } else {
                    captured.extend_from_slice(
                        &bytes[..bytes.len().min(STDERR_LIMIT - captured.len())],
                    );
                }
            }
            Some(run_command_response::Event::Completed(result)) => return Ok(result.outcome),
            _ => return Err(unknown("unexpected sandboxd command event")),
        }
    }
    Err(unknown("sandboxd command ended without a terminal result"))
}

async fn cancel_and_wait(
    client: &mut SandboxdClient<Channel>,
    events: &mut tonic::Streaming<RunCommandResponse>,
) -> Result<Option<Outcome>, SandboxTransportError> {
    tokio::time::timeout(CANCEL_ANSWER_TIMEOUT, async {
        // A completed result can race cancellation, even if the Cancel RPC fails.
        let _ = tokio::time::timeout(
            CONTROL_TIMEOUT,
            client.cancel(CancelRequest {
                call_id: CALL_ID.into(),
            }),
        )
        .await;
        while let Some(event) = events.message().await.map_err(unknown)? {
            if let Some(run_command_response::Event::Completed(result)) = event.event {
                return Ok(result.outcome);
            }
        }
        Err(unknown(
            "sandboxd ended before command termination was confirmed",
        ))
    })
    .await
    .unwrap_or_else(|_| Err(unknown("sandboxd did not confirm command termination")))
}

fn outcome(
    terminal: Option<Outcome>,
    stderr: Vec<u8>,
) -> Result<SandboxStreamOutcome, SandboxTransportError> {
    match terminal {
        Some(Outcome::ExitCode(exit_code)) => Ok(SandboxStreamOutcome { exit_code, stderr }),
        Some(Outcome::Rejected(detail) | Outcome::Failed(detail)) => {
            Err(SandboxTransportError::Failed(detail))
        }
        Some(Outcome::Cancelled(true)) => Err(SandboxTransportError::Failed(
            "command cancelled; its process group stopped".into(),
        )),
        Some(Outcome::DeadlineExceeded(true)) => Err(SandboxTransportError::Failed(
            "command deadline exceeded; its process group stopped".into(),
        )),
        Some(Outcome::Cancelled(false) | Outcome::DeadlineExceeded(false)) => {
            Err(unknown("command process group was not observed stopped"))
        }
        None => Err(unknown("sandboxd command result has no outcome")),
    }
}

fn unknown(error: impl std::fmt::Display) -> SandboxTransportError {
    SandboxTransportError::OutcomeUnknown(error.to_string())
}
