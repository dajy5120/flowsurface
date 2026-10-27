use crate::error::AdapterError;

use bytes::Bytes;
use fastwebsockets::FragmentCollector;
use futures::{Stream as FuturesStream, channel::mpsc};
use http_body_util::Empty;
use hyper::{
    Request,
    header::{CONNECTION, UPGRADE},
    upgrade::Upgraded,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::{
    future::Future,
    pin::Pin,
    sync::LazyLock,
    task::{Context, Poll},
    time::Duration,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, OwnedTrustAnchor},
};
use url::Url;

const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

pub static TLS_CONNECTOR: LazyLock<TlsConnector> = LazyLock::new(tls_connector);

fn tls_connector() -> TlsConnector {
    let mut root_store = tokio_rustls::rustls::RootCertStore::empty();

    root_store.add_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.0.iter().map(|ta| {
        OwnedTrustAnchor::from_subject_spki_name_constraints(
            ta.subject,
            ta.spki,
            ta.name_constraints,
        )
    }));

    let config = ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    TlsConnector::from(std::sync::Arc::new(config))
}

pub enum State {
    Disconnected,
    Connected(FragmentCollector<TokioIo<Upgraded>>),
}

/// 「连着但没数据」看门狗：这么久一个数据帧都没收到，就当断了、重连。
///
/// 2026-10-27 币安永续的 `@kline_1m` 停推了几分钟——连接一直在、ping 照常，
/// 图却停着，状态栏也不报，因为原来只认「连接断了 / 读出错」两种断线。
///
/// 门限按流的种类定，依据是币安成交最少的三个永续实测（同日）：
/// - 盘口：最长 29 秒才推一次 → [`DEPTH_STALE_AFTER`] 120 秒；
/// - K 线：每根新 K 线开始时必推一次（1m 最长正好 60 秒）→ [`kline_stale_after`]；
/// - 逐笔成交：90 秒一笔都没有是常态 → **不设**，冷门币会被当成中断反复重连。
pub const DEPTH_STALE_AFTER: Duration = Duration::from_secs(120);

/// K 线连接的门限：最小周期 + 60 秒，至少 120 秒。一条连接里订了几个周期时，
/// 最小那个的换根就保证了至少这么久推一次。
pub fn kline_stale_after<T>(streams: &[(T, crate::Timeframe)]) -> Option<Duration> {
    let min_ms = streams.iter().map(|(_, tf)| tf.to_milliseconds()).min()?;
    Some(Duration::from_millis(min_ms + 60_000).max(Duration::from_secs(120)))
}

/// 看门狗断线的原因文字都以它开头；界面据此把它和普通断线分开
/// （普通断线重连上就算好了，这种要等真收到数据才算好）。
pub const STALE_REASON: &str = "行情中断";

#[derive(Debug)]
pub enum ReadError {
    Ws(fastwebsockets::WebSocketError),
    Stale(Duration),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Ws(e) => write!(f, "{e}"),
            ReadError::Stale(d) => {
                write!(f, "{STALE_REASON}：{} 秒没收到数据（连接还在），重连", d.as_secs())
            }
        }
    }
}

/// 读下一个**数据帧**（Text / Binary / Close）；ping / pong 是控制帧，
/// 照常由 fastwebsockets 自动回应，但不算数据、不推迟看门狗。
/// `stale_after` 为 `None` 时不设看门狗（逐笔成交）。
pub async fn read_data_frame<'f, S>(
    ws: &mut FragmentCollector<S>,
    stale_after: Option<Duration>,
) -> Result<fastwebsockets::Frame<'f>, ReadError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    read_data_frame_since(ws, tokio::time::Instant::now(), stale_after).await
}

/// 同 [`read_data_frame`]，从 `since`（上次收到数据的时刻）起算——读帧会被别的分支打断
/// （MEXC 在 `select!` 里定时发 ping）的地方用它，否则每次 ping 都会把计时清零。
pub async fn read_data_frame_since<'f, S>(
    ws: &mut FragmentCollector<S>,
    since: tokio::time::Instant,
    stale_after: Option<Duration>,
) -> Result<fastwebsockets::Frame<'f>, ReadError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use fastwebsockets::OpCode;
    loop {
        let r = match stale_after {
            Some(d) => match tokio::time::timeout_at(since + d, ws.read_frame()).await {
                Err(_) => return Err(ReadError::Stale(d)),
                Ok(r) => r,
            },
            None => ws.read_frame().await,
        };
        match r {
            Err(e) => return Err(ReadError::Ws(e)),
            Ok(f) if matches!(f.opcode, OpCode::Ping | OpCode::Pong) => continue,
            Ok(f) => return Ok(f),
        }
    }
}

struct ChannelStream<T> {
    receiver: mpsc::Receiver<T>,
    task: tokio::task::JoinHandle<()>,
}

impl<T> FuturesStream for ChannelStream<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.receiver).poll_next(cx)
    }
}

impl<T> Drop for ChannelStream<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn channel<T, Fut, F>(buffer: usize, f: F) -> impl futures::Stream<Item = T>
where
    T: Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
    F: FnOnce(mpsc::Sender<T>) -> Fut + Send + 'static,
{
    let (sender, receiver) = mpsc::channel(buffer);
    let task = tokio::spawn(async move {
        f(sender).await;
    });

    ChannelStream { receiver, task }
}

pub async fn connect_ws(
    domain: &str,
    url: &str,
    proxy_cfg: Option<&super::proxy::Proxy>,
) -> Result<FragmentCollector<TokioIo<Upgraded>>, AdapterError> {
    let parsed = Url::parse(url).map_err(|e| AdapterError::InvalidRequest(e.to_string()))?;

    let url_host = parsed
        .host_str()
        .ok_or_else(|| AdapterError::InvalidRequest("Missing host in websocket URL".to_string()))?;

    if !url_host.eq_ignore_ascii_case(domain) {
        return Err(AdapterError::InvalidRequest(format!(
            "WebSocket URL host mismatch: url_host={url_host}, domain_arg={domain}"
        )));
    }

    let target_port = parsed.port_or_known_default().ok_or_else(|| {
        AdapterError::InvalidRequest("Missing port for websocket URL".to_string())
    })?;

    let stream = setup_tcp(domain, target_port, proxy_cfg).await?;

    match parsed.scheme() {
        "wss" => {
            let tls_stream =
                tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, upgrade_to_tls(domain, stream))
                    .await
                    .map_err(|_| {
                        AdapterError::WebsocketError(
                            "TLS handshake to target timed out".to_string(),
                        )
                    })??;

            tokio::time::timeout(
                WS_HANDSHAKE_TIMEOUT,
                upgrade_to_websocket(domain, tls_stream, &parsed),
            )
            .await
            .map_err(|_| {
                AdapterError::WebsocketError("WebSocket handshake timed out".to_string())
            })?
        }
        "ws" => tokio::time::timeout(
            WS_HANDSHAKE_TIMEOUT,
            upgrade_to_websocket(domain, stream, &parsed),
        )
        .await
        .map_err(|_| AdapterError::WebsocketError("WebSocket handshake timed out".to_string()))?,
        _ => Err(AdapterError::InvalidRequest(
            "Invalid scheme for websocket URL".to_string(),
        )),
    }
}

async fn setup_tcp(
    domain: &str,
    target_port: u16,
    proxy_cfg: Option<&super::proxy::Proxy>,
) -> Result<super::proxy::ProxyStream, AdapterError> {
    if let Some(proxy) = proxy_cfg {
        log::info!("Using proxy for WS: {}", proxy);
        return proxy.connect_tcp(domain, target_port).await;
    }

    let addr = format!("{domain}:{target_port}");
    let tcp = tokio::time::timeout(TCP_CONNECT_TIMEOUT, tokio::net::TcpStream::connect(&addr))
        .await
        .map_err(|_| AdapterError::WebsocketError(format!("TCP connect timeout: {addr}")))?
        .map_err(|e| AdapterError::WebsocketError(e.to_string()))?;

    Ok(super::proxy::ProxyStream::Plain(tcp))
}

async fn upgrade_to_tls<S>(
    domain: &str,
    stream: S,
) -> Result<tokio_rustls::client::TlsStream<S>, AdapterError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let domain: tokio_rustls::rustls::ServerName =
        tokio_rustls::rustls::ServerName::try_from(domain)
            .map_err(|_| AdapterError::ParseError("invalid dnsname".to_string()))?;

    TLS_CONNECTOR
        .connect(domain, stream)
        .await
        .map_err(|e| AdapterError::WebsocketError(e.to_string()))
}

async fn upgrade_to_websocket<S>(
    domain: &str,
    stream: S,
    parsed: &Url,
) -> Result<FragmentCollector<TokioIo<Upgraded>>, AdapterError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut path_and_query = parsed.path().to_string();
    if let Some(q) = parsed.query() {
        path_and_query.push('?');
        path_and_query.push_str(q);
    }
    if path_and_query.is_empty() {
        path_and_query.push('/');
    }

    let host_header = match parsed.port() {
        Some(explicit_port) => {
            let default_port = parsed.port_or_known_default().unwrap_or(explicit_port);
            if explicit_port != default_port {
                format!("{domain}:{explicit_port}")
            } else {
                domain.to_string()
            }
        }
        None => domain.to_string(),
    };

    let req: Request<Empty<Bytes>> = Request::builder()
        .method("GET")
        .uri(path_and_query)
        .header("Host", host_header)
        .header(UPGRADE, "websocket")
        .header(CONNECTION, "upgrade")
        .header(
            "Sec-WebSocket-Key",
            fastwebsockets::handshake::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13")
        .body(Empty::<Bytes>::new())
        .map_err(|e| AdapterError::WebsocketError(e.to_string()))?;

    let exec = TokioExecutor::new();
    let (ws, _) = fastwebsockets::handshake::client(&exec, req, stream)
        .await
        .map_err(|e| AdapterError::WebsocketError(e.to_string()))?;

    Ok(FragmentCollector::new(ws))
}

#[cfg(test)]
mod watchdog_tests {
    use super::*;
    use fastwebsockets::{Frame, OpCode, Payload, Role, WebSocket};

    fn pair() -> (
        FragmentCollector<tokio::io::DuplexStream>,
        WebSocket<tokio::io::DuplexStream>,
    ) {
        let (a, b) = tokio::io::duplex(1 << 16);
        (
            FragmentCollector::new(WebSocket::after_handshake(a, Role::Client)),
            WebSocket::after_handshake(b, Role::Server),
        )
    }

    fn ms(n: u64) -> Option<Duration> {
        Some(Duration::from_millis(n))
    }

    #[tokio::test]
    async fn pings_alone_do_not_keep_the_watchdog_quiet() {
        // 这次币安的样子：连接在、ping 照来，就是没有数据
        let (mut client, mut server) = pair();
        let srv = tokio::spawn(async move {
            for _ in 0..8 {
                if server.write_frame(Frame::new(true, OpCode::Ping, None, Payload::Owned(vec![1]))).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        });
        let r = read_data_frame(&mut client, ms(200)).await;
        assert!(matches!(r, Err(ReadError::Stale(_))), "{:?}", r.map(|f| f.opcode));
        let msg = r.err().unwrap().to_string();
        assert!(msg.starts_with(STALE_REASON), "{msg}");
        srv.abort();
    }

    #[test]
    fn kline_threshold_follows_the_smallest_timeframe() {
        use crate::Timeframe;
        assert_eq!(kline_stale_after(&[((), Timeframe::M1)]), Some(Duration::from_secs(120)));
        assert_eq!(kline_stale_after(&[((), Timeframe::H1), ((), Timeframe::M5)]), Some(Duration::from_secs(360)));
        assert_eq!(kline_stale_after::<()>(&[]), None);
    }

    #[tokio::test]
    async fn data_after_pings_is_returned() {
        let (mut client, mut server) = pair();
        server.write_frame(Frame::new(true, OpCode::Ping, None, Payload::Owned(vec![1]))).await.unwrap();
        server.write_frame(Frame::text(Payload::Owned(b"{\"k\":1}".to_vec()))).await.unwrap();
        let f = read_data_frame(&mut client, ms(1000)).await.unwrap();
        assert_eq!(f.opcode, OpCode::Text);
        assert_eq!(&f.payload[..], b"{\"k\":1}");
    }

    #[tokio::test]
    async fn close_is_returned_not_swallowed() {
        let (mut client, mut server) = pair();
        server.write_frame(Frame::close(1000, b"bye")).await.unwrap();
        let f = read_data_frame(&mut client, ms(1000)).await.unwrap();
        assert_eq!(f.opcode, OpCode::Close);
    }
}
