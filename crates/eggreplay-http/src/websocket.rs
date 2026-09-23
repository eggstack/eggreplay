//! Raw WebSocket codec adapters over streams already owned by EggFetch/EggServe.
//!
//! This module intentionally exposes no connect/accept network helpers. The
//! caller must complete HTTP ownership and the RFC 6455 handshake first.

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite::{self, protocol::Role};

/// A WebSocket codec stream over a caller-owned async byte stream.
pub type WebSocketStream<S> = tokio_tungstenite::WebSocketStream<S>;

/// Semantic WebSocket message type.
pub type Message = tungstenite::Message;

/// WebSocket codec configuration.
pub type WebSocketConfig = tungstenite::protocol::WebSocketConfig;

/// Codec or protocol error.
pub type Error = tungstenite::Error;

/// Create a raw client-role codec over an already-upgraded stream.
pub async fn client<S>(stream: S, config: Option<WebSocketConfig>) -> WebSocketStream<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    WebSocketStream::from_raw_socket(stream, Role::Client, config).await
}

/// Create a raw server-role codec over an already-upgraded stream.
pub async fn server<S>(stream: S, config: Option<WebSocketConfig>) -> WebSocketStream<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    WebSocketStream::from_raw_socket(stream, Role::Server, config).await
}

/// Derive the RFC 6455 accept key using the codec's maintained helper.
pub fn derive_accept_key(key: &[u8]) -> String {
    tungstenite::handshake::derive_accept_key(key)
}

/// Validate an RFC 6455 client key's encoding and decoded length.
pub fn valid_client_key(key: &str) -> bool {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(key)
        .is_ok_and(|decoded| decoded.len() == 16)
}

/// Send one semantic message and flush immediately for direct backpressure.
pub async fn send<S>(stream: &mut WebSocketStream<S>, message: Message) -> Result<(), Error>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    stream.send(message).await
}

/// Read one semantic message from a stream.
pub async fn next<S>(stream: &mut WebSocketStream<S>) -> Option<Result<Message, Error>>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    stream.next().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    struct LeadingIo<T> {
        prefix: &'static [u8],
        offset: usize,
        inner: T,
    }

    impl<T: AsyncRead + Unpin> AsyncRead for LeadingIo<T> {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.offset < self.prefix.len() {
                let count = (self.prefix.len() - self.offset).min(buf.remaining());
                let start = self.offset;
                buf.put_slice(&self.prefix[start..start + count]);
                self.offset += count;
                return std::task::Poll::Ready(Ok(()));
            }
            std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl<T: AsyncWrite + Unpin> AsyncWrite for LeadingIo<T> {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn raw_client_server_roundtrip_text_binary_ping_and_close() {
        let (left, right) = duplex(4096);
        let mut client = client(left, None).await;
        let mut server = server(right, None).await;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            send(&mut client, Message::Text("hello".into()))
                .await
                .unwrap();
            assert_eq!(
                next(&mut server).await.unwrap().unwrap(),
                Message::Text("hello".into())
            );
            send(&mut client, Message::Binary(vec![0, 1, 2].into()))
                .await
                .unwrap();
            assert_eq!(
                next(&mut server).await.unwrap().unwrap(),
                Message::Binary(vec![0, 1, 2].into())
            );
            send(&mut client, Message::Ping(vec![3, 4].into()))
                .await
                .unwrap();
            assert_eq!(
                next(&mut server).await.unwrap().unwrap(),
                Message::Ping(vec![3, 4].into())
            );
            // Tungstenite auto-queues a Pong when the ping is read. Sending a
            // semantic Pong explicitly would duplicate this protocol response.
            server.flush().await.unwrap();
            assert_eq!(
                next(&mut client).await.unwrap().unwrap(),
                Message::Pong(vec![3, 4].into())
            );
            send(&mut server, Message::Close(None)).await.unwrap();
            assert!(matches!(
                next(&mut client).await.unwrap().unwrap(),
                Message::Close(_)
            ));
            client.flush().await.unwrap();
            assert!(matches!(
                next(&mut server).await.unwrap().unwrap(),
                Message::Close(_)
            ));
        })
        .await
        .expect("codec roundtrip timed out");
    }

    #[tokio::test]
    async fn fragmented_text_is_reassembled_as_one_semantic_message() {
        use tungstenite::protocol::frame::{Frame, coding::Data};

        let (left, right) = duplex(4096);
        let mut client = client(left, None).await;
        let mut server = server(right, None).await;
        send(
            &mut client,
            Message::Frame(Frame::message(
                "hel",
                tungstenite::protocol::frame::coding::OpCode::Data(Data::Text),
                false,
            )),
        )
        .await
        .unwrap();
        send(
            &mut client,
            Message::Frame(Frame::message(
                "lo",
                tungstenite::protocol::frame::coding::OpCode::Data(Data::Continue),
                true,
            )),
        )
        .await
        .unwrap();
        assert_eq!(
            next(&mut server).await.unwrap().unwrap(),
            Message::Text("hello".into())
        );
    }

    #[tokio::test]
    async fn codec_reads_leading_frame_bytes_from_upgraded_stream_adapter() {
        let (_left, right) = duplex(64);
        let upgraded_like = LeadingIo {
            // Unmasked server-to-client text frame containing `hello`.
            prefix: b"\x81\x05hello",
            offset: 0,
            inner: right,
        };
        let mut client = client(upgraded_like, None).await;
        assert_eq!(
            next(&mut client).await.unwrap().unwrap(),
            Message::Text("hello".into())
        );
    }

    #[tokio::test]
    async fn codec_rejects_oversized_message_at_configured_bound() {
        let (left, right) = duplex(4096);
        let mut client = client(left, None).await;
        let mut server = server(
            right,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(2))
                    .max_frame_size(Some(2)),
            ),
        )
        .await;
        send(&mut client, Message::Binary(vec![1, 2, 3].into()))
            .await
            .unwrap();
        assert!(matches!(
            next(&mut server).await.unwrap(),
            Err(Error::Capacity(_))
        ));
    }

    #[test]
    fn handshake_helpers_validate_key_and_derive_accept() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        assert!(valid_client_key(key));
        assert!(!valid_client_key("bad"));
        assert_eq!(
            derive_accept_key(key.as_bytes()),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }
}
