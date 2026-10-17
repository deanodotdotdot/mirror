use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener as StdListener};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures_util::future::BoxFuture;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::*;

/// Only the dedicated login UI may reveal this transient value.
pub struct ClaudeAuthorizationUrl(Secret);
impl ClaudeAuthorizationUrl {
    pub fn expose_for_login_ui(&self) -> &str {
        self.0.expose()
    }
}
impl fmt::Debug for ClaudeAuthorizationUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClaudeAuthorizationUrl([REDACTED])")
    }
}

/// Never serialize, display, or log callback input.
pub struct ClaudeCallbackInput(Secret);
impl ClaudeCallbackInput {
    pub fn new(value: String) -> Result<Self, ClaudeError> {
        if value.len() > 8192 {
            return Err(ClaudeError::new(ClaudeErrorKind::InvalidConfiguration));
        }
        Ok(Self(Secret(value)))
    }
}
impl fmt::Debug for ClaudeCallbackInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClaudeCallbackInput([REDACTED])")
    }
}

#[derive(Debug)]
pub enum ClaudeLoginEvent {
    Progress {
        phase: Phase,
        callback_rejected: bool,
    },
    AuthorizationUrl {
        authorization_url: ClaudeAuthorizationUrl,
        expires_at: SystemTime,
    },
}

struct PendingLogin {
    state: Secret,
    verifier: Secret,
    redirect: String,
    deadline: Instant,
    expires_at: SystemTime,
}
impl PendingLogin {
    fn new(redirect: String) -> Result<Self, ClaudeError> {
        fn random<const N: usize>() -> Result<Secret, ClaudeError> {
            let mut bytes = [0; N];
            SystemRandom::new()
                .fill(&mut bytes)
                .map_err(|_| temporary_error())?;
            Ok(Secret(URL_SAFE_NO_PAD.encode(bytes)))
        }
        Ok(Self {
            state: random::<32>()?,
            verifier: random::<64>()?,
            redirect,
            deadline: Instant::now() + Duration::from_secs(300),
            expires_at: SystemTime::now() + Duration::from_secs(300),
        })
    }
    fn url(&self) -> ClaudeAuthorizationUrl {
        let digest = ring::digest::digest(&ring::digest::SHA256, self.verifier.expose().as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest.as_ref());
        let mut url =
            reqwest::Url::parse(profile::AUTHORIZATION_URL).expect("frozen authorization URL");
        url.query_pairs_mut().extend_pairs([
            ("client_id", profile::CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", self.redirect.as_str()),
            ("scope", &profile::SCOPES.join(" ")),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", self.state.expose()),
            ("code", "true"),
        ]);
        ClaudeAuthorizationUrl(Secret(url.into()))
    }
    fn parse(&self, value: &str) -> Option<Secret> {
        if value.len() > 8192 || Instant::now() >= self.deadline {
            return None;
        }
        let value = value.trim();
        let code = if value.contains("://") {
            let url = reqwest::Url::parse(value).ok()?;
            let expected = reqwest::Url::parse(&self.redirect).ok()?;
            if url.scheme() != expected.scheme()
                || url.host_str() != expected.host_str()
                || url.port() != expected.port()
                || url.path() != expected.path()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return None;
            }
            let fields: Vec<_> = url.query_pairs().collect();
            if fields.len() > 12 || fields.iter().any(|(key, _)| key == "error") {
                return None;
            }
            let values = |key: &str| {
                fields
                    .iter()
                    .filter(|(name, _)| name == key)
                    .map(|(_, value)| value.as_ref())
                    .collect::<Vec<_>>()
            };
            let states = values("state");
            if states.len() != 1 || states[0] != self.state.expose() {
                return None;
            }
            let codes = values("code");
            if codes.len() != 1 {
                return None;
            }
            codes[0].to_owned()
        } else {
            value.to_owned()
        };
        let code = if let Some((code, state)) = code.split_once('#') {
            if state != self.state.expose() {
                return None;
            }
            code
        } else {
            &code
        };
        if !secret_valid(code) || code.starts_with("sk-ant-") {
            return None;
        }
        Some(Secret(code.to_owned()))
    }
}

struct Callbacks {
    ipv4: TcpListener,
    ipv6: Option<TcpListener>,
    redirect: String,
}
impl Callbacks {
    fn bind() -> Result<Self, ClaudeError> {
        for attempt in 0..10 {
            let port = if attempt == 0 {
                profile::CALLBACK_PORT
            } else {
                0
            };
            let Ok(ipv4) = StdListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))) else {
                continue;
            };
            let port = ipv4.local_addr().map_err(|_| temporary_error())?.port();
            let ipv6 = match StdListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))) {
                Ok(ipv6) => Some(ipv6),
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(
                            nix::libc::EAFNOSUPPORT
                                | nix::libc::EPROTONOSUPPORT
                                | nix::libc::EADDRNOTAVAIL
                        )
                    ) =>
                {
                    None
                }
                Err(_) => continue,
            };
            ipv4.set_nonblocking(true).map_err(|_| temporary_error())?;
            if let Some(ipv6) = &ipv6 {
                ipv6.set_nonblocking(true).map_err(|_| temporary_error())?;
            }
            return Ok(Self {
                ipv4: TcpListener::from_std(ipv4).map_err(|_| temporary_error())?,
                ipv6: ipv6
                    .map(TcpListener::from_std)
                    .transpose()
                    .map_err(|_| temporary_error())?,
                redirect: format!("http://localhost:{port}/callback"),
            });
        }
        Err(temporary_error())
    }
    async fn accept(&self) -> std::io::Result<TcpStream> {
        tokio::select! {
            result = self.ipv4.accept() => result.map(|(stream,_)| stream),
            result = async { match &self.ipv6 { Some(listener) => listener.accept().await, None => std::future::pending().await } } => result.map(|(stream,_)| stream),
        }
    }
}

async fn receive_callback(mut stream: TcpStream, pending: &PendingLogin) -> Option<Secret> {
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut bytes = Vec::new();
        let code = loop {
            if bytes.len() > 8192 { break None; }
            if bytes.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let Some(text) = std::str::from_utf8(&bytes).ok() else { break None; };
                let mut lines = text.split("\r\n");
                let Some(line) = lines.next() else { break None; };
                let parts: Vec<_> = line.split(' ').collect();
                let expected = reqwest::Url::parse(&pending.redirect).ok()?;
                let host = format!("localhost:{}", expected.port()?);
                let hosts: Vec<_> = lines.filter_map(|line| line.split_once(':')).filter(|(name,_)| name.eq_ignore_ascii_case("host")).map(|(_,value)| value.trim()).collect();
                if parts.len() != 3 || parts[0] != "GET" || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1") || hosts != [host.as_str()] || !parts[1].starts_with("/callback?") { break None; }
                break pending.parse(&format!("http://{host}{}", parts[1]));
            }
            let mut chunk = [0; 1024];
            let size = stream.read(&mut chunk).await.ok()?;
            if size == 0 { break None; }
            bytes.extend_from_slice(&chunk[..size]);
        };
        let status = if code.is_some() { "200 OK" } else { "400 Bad Request" };
        let body = "Return to the Mirror login terminal.";
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let _ = tokio::time::timeout(Duration::from_millis(100), stream.write_all(response.as_bytes())).await;
        code
    }).await.ok().flatten()
}

impl ClaudeAuthManager {
    pub async fn login(
        &self,
        mut input: mpsc::Receiver<ClaudeCallbackInput>,
        events: mpsc::Sender<ClaudeLoginEvent>,
        cancellation: BoxFuture<'_, ()>,
    ) -> Result<ClaudeAuthStatus, CommandError> {
        let phase = Mutex::new(Phase::Preparing);
        let emit = |next, rejected| {
            *phase.lock().expect("phase mutex") = next;
            events
                .try_send(ClaudeLoginEvent::Progress {
                    phase: next,
                    callback_rejected: rejected,
                })
                .map_err(|_| {
                    CommandError::new(ClaudeError::new(ClaudeErrorKind::LoginCancelled), next)
                })
        };
        let run = async {
            oauth::check_environment()
                .map_err(|error| CommandError::new(error, Phase::Preparing))?;
            emit(Phase::Preparing, false)?;
            let starting = {
                let locked = self
                    .store
                    .lock(true)
                    .await
                    .map_err(|error| CommandError::new(error, Phase::Preparing))?
                    .ok_or_else(|| CommandError::new(storage_error(), Phase::Preparing))?;
                locked
                    .load()
                    .map_err(|error| CommandError::new(error, Phase::Preparing))?
            };
            let callbacks =
                Callbacks::bind().map_err(|error| CommandError::new(error, Phase::Preparing))?;
            let pending = PendingLogin::new(callbacks.redirect.clone())
                .map_err(|error| CommandError::new(error, Phase::Preparing))?;
            events
                .try_send(ClaudeLoginEvent::AuthorizationUrl {
                    authorization_url: pending.url(),
                    expires_at: pending.expires_at,
                })
                .map_err(|_| {
                    CommandError::new(
                        ClaudeError::new(ClaudeErrorKind::LoginCancelled),
                        Phase::Preparing,
                    )
                })?;
            emit(Phase::AwaitingCallback, false)?;
            let complete = async {
                let code = loop {
                    let code = tokio::select! {
                        incoming = input.recv() => match incoming {
                            Some(value) => pending.parse(value.0.expose()),
                            None => return Err(CommandError::new(ClaudeError::new(ClaudeErrorKind::LoginCancelled), Phase::AwaitingCallback)),
                        },
                        stream = callbacks.accept() => match stream { Ok(stream) => receive_callback(stream, &pending).await, Err(_) => None },
                    };
                    if let Some(code) = code {
                        break code;
                    }
                    emit(Phase::AwaitingCallback, true)?;
                };
                // One winner consumes this pending flow before any exchange await.
                emit(Phase::ExchangingCode, false)?;
                let mut data = tokio::time::timeout(
                    Duration::from_secs(10),
                    self.transport.token(TokenRequest::Exchange {
                        code: &code,
                        state: &pending.state,
                        verifier: &pending.verifier,
                        redirect: &pending.redirect,
                    }),
                )
                .await
                .map_err(|_| CommandError::new(temporary_error(), Phase::ExchangingCode))?
                .map_err(|error| CommandError::new(error, Phase::ExchangingCode))?;
                let issued_at = SystemTime::now();
                // Validate token kind and every provided identity before bootstrap.
                let object = data
                    .as_object_mut()
                    .ok_or_else(|| CommandError::new(invalid_token(), Phase::ExchangingCode))?;
                for key in ["account", "organization"] {
                    if let Some(value) = object.get(key) {
                        let object = value.as_object().ok_or_else(|| {
                            CommandError::new(invalid_token(), Phase::ResolvingIdentity)
                        })?;
                        if let Some(value) = object.get("uuid") {
                            if value
                                .as_str()
                                .and_then(|v| v.parse::<Uuid>().ok())
                                .is_none_or(|id| id.is_nil())
                            {
                                return Err(CommandError::new(
                                    invalid_token(),
                                    Phase::ResolvingIdentity,
                                ));
                            }
                        }
                    }
                }
                let mut validation = data.clone();
                for key in ["account", "organization"] {
                    if validation.get(key).and_then(|v| v.get("uuid")).is_none() {
                        validation.as_object_mut().unwrap().insert(
                            key.into(),
                            serde_json::json!({"uuid":"11111111-1111-4111-8111-111111111111"}),
                        );
                    }
                }
                let validated = Credential::from_token(&validation, None, issued_at)
                    .map_err(|error| CommandError::new(error, Phase::ExchangingCode))?;
                if ["account", "organization"]
                    .iter()
                    .any(|key| data.get(key).and_then(|v| v.get("uuid")).is_none())
                {
                    emit(Phase::ResolvingIdentity, false)?;
                    let bootstrap = tokio::time::timeout(
                        Duration::from_secs(10),
                        self.transport.bootstrap(&validated.access_token),
                    )
                    .await
                    .map_err(|_| CommandError::new(temporary_error(), Phase::ResolvingIdentity))?
                    .map_err(|error| CommandError::new(error, Phase::ResolvingIdentity))?;
                    let account = bootstrap
                        .get("oauth_account")
                        .and_then(Value::as_object)
                        .ok_or_else(|| {
                            CommandError::new(invalid_token(), Phase::ResolvingIdentity)
                        })?;
                    for (key, field) in [
                        ("account", "account_uuid"),
                        ("organization", "organization_uuid"),
                    ] {
                        let value = account
                            .get(field)
                            .and_then(Value::as_str)
                            .and_then(|text| text.parse::<Uuid>().ok())
                            .filter(|id| !id.is_nil())
                            .ok_or_else(|| {
                                CommandError::new(invalid_token(), Phase::ResolvingIdentity)
                            })?;
                        if let Some(existing) = data
                            .get(key)
                            .and_then(|v| v.get("uuid"))
                            .and_then(Value::as_str)
                        {
                            if existing.parse::<Uuid>().ok() != Some(value) {
                                return Err(CommandError::new(
                                    invalid_token(),
                                    Phase::ResolvingIdentity,
                                ));
                            }
                        } else {
                            let object = data
                                .as_object_mut()
                                .unwrap()
                                .entry(key)
                                .or_insert_with(|| serde_json::json!({}));
                            object
                                .as_object_mut()
                                .unwrap()
                                .insert("uuid".into(), serde_json::json!(value));
                        }
                    }
                }
                let credential = Credential::from_token(&data, None, issued_at)
                    .map_err(|error| CommandError::new(error, Phase::ResolvingIdentity))?;
                emit(Phase::Committing, false)?;
                let locked = self
                    .store
                    .lock(true)
                    .await
                    .map_err(|error| CommandError::new(error, Phase::Committing))?
                    .ok_or_else(|| CommandError::new(storage_error(), Phase::Committing))?;
                let current = locked
                    .load()
                    .map_err(|error| CommandError::new(error, Phase::Committing))?;
                if current.as_ref().map(|v| v.generation) != starting.as_ref().map(|v| v.generation)
                {
                    return Err(CommandError::new(
                        ClaudeError::new(ClaudeErrorKind::LoginCancelled),
                        Phase::Committing,
                    ));
                }
                let mut next = Record::tombstone(current.as_ref());
                next.revision = 1;
                next.credential = Some(credential);
                locked.write(&next)?;
                Ok(snapshot(Some(&next)).status)
            };
            tokio::time::timeout_at(pending.deadline, complete)
                .await
                .map_err(|_| {
                    CommandError::new(
                        ClaudeError::new(ClaudeErrorKind::LoginTimeout),
                        *phase.lock().expect("phase mutex"),
                    )
                })?
        };
        tokio::select! { biased;
            _ = cancellation => Err(CommandError::new(ClaudeError::new(ClaudeErrorKind::LoginCancelled), *phase.lock().expect("phase mutex"))),
            result = run => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_validates_all_states_and_never_accepts_credentials() {
        let pending = PendingLogin::new("http://localhost:54545/callback".into()).unwrap();
        let state = pending.state.expose();
        assert!(pending.parse("code").is_some());
        assert!(pending.parse(&format!("code#{state}")).is_some());
        assert!(pending
            .parse(&format!(
                "http://localhost:54545/callback?state={state}&code=code%23{state}"
            ))
            .is_some());
        for bad in [
            "sk-ant-oat-secret",
            "code#wrong",
            "http://evil.invalid/callback?code=secret",
            "code with spaces",
        ] {
            assert!(pending.parse(bad).is_none());
        }
        assert!(pending
            .parse(&format!(
                "http://localhost:54545/callback?state={state}&state={state}&code=code"
            ))
            .is_none());
        assert!(pending
            .parse(&format!(
                "http://localhost:54545/callback?state={state}&code=code%23wrong"
            ))
            .is_none());
        assert!(!format!("{:?}", pending.url()).contains(state));
    }

    #[tokio::test]
    async fn actual_dual_loopback_callbacks_and_occupied_port_fallback() {
        // If a parallel login owns the preferred port it is already occupied.
        let _occupied = StdListener::bind((Ipv4Addr::LOCALHOST, profile::CALLBACK_PORT)).ok();
        let callbacks = Callbacks::bind().unwrap();
        let pending = PendingLogin::new(callbacks.redirect.clone()).unwrap();
        let port = reqwest::Url::parse(&pending.redirect)
            .unwrap()
            .port()
            .unwrap();
        assert_ne!(port, profile::CALLBACK_PORT);
        let mut addresses = vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))];
        if callbacks.ipv6.is_some() {
            addresses.push(SocketAddr::from((Ipv6Addr::LOCALHOST, port)));
        }
        for address in addresses {
            let client = async {
                let mut stream = TcpStream::connect(address).await.unwrap();
                let request=format!("GET /callback?code=SYNTHETIC_CODE&state={} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n",pending.state.expose());
                stream.write_all(request.as_bytes()).await.unwrap();
                let mut response = Vec::new();
                stream.read_to_end(&mut response).await.unwrap();
                assert!(response.starts_with(b"HTTP/1.1 200"));
                assert!(!String::from_utf8(response)
                    .unwrap()
                    .contains("SYNTHETIC_CODE"));
            };
            let server = async {
                receive_callback(callbacks.accept().await.unwrap(), &pending)
                    .await
                    .unwrap()
            };
            let (code, _) = tokio::join!(server, client);
            assert_eq!(code.expose(), "SYNTHETIC_CODE");
        }
    }

    #[tokio::test]
    async fn slow_callback_has_absolute_connection_budget() {
        let callbacks = Callbacks::bind().unwrap();
        let pending = PendingLogin::new(callbacks.redirect.clone()).unwrap();
        let port = reqwest::Url::parse(&pending.redirect)
            .unwrap()
            .port()
            .unwrap();
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        stream.write_all(b"G").await.unwrap();
        let started = Instant::now();
        assert!(
            receive_callback(callbacks.accept().await.unwrap(), &pending)
                .await
                .is_none()
        );
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert!(started.elapsed() < Duration::from_secs(2));
        let mut pending = pending;
        pending.deadline = Instant::now() - Duration::from_secs(1);
        assert!(pending.parse("SYNTHETIC_CODE").is_none());
    }
}
