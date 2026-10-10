use super::http_plain_proxy;
use crate::config::NetworkProxyConfig;
use crate::runtime::network_proxy_state_for_policy;
use pretty_assertions::assert_eq;
use rama_core::extensions::ExtensionsMut;
use rama_http::Body;
use rama_http::Request;
use rama_http::StatusCode;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::time::timeout;

#[tokio::test]
async fn exact_registered_linux_socket_relays_http_without_exposing_transport_header() {
    let directory = tempfile::tempdir().unwrap();
    let socket_path = directory.path().join("broker.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0; 1024];
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0 && bytes.len() < 8192);
            bytes.extend_from_slice(&buffer[..count]);
            if bytes.windows(4).any(|end| end == b"\r\n\r\n") {
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8(bytes).unwrap()
    });
    let mut network = NetworkProxyConfig::default();
    network.set_allow_unix_sockets(vec![socket_path.to_str().unwrap().to_string()]);
    let state = Arc::new(network_proxy_state_for_policy(network));
    let mut request = Request::builder()
        .method("POST")
        .uri("http://tomako-broker.invalid/capability?revision=1")
        .header("host", "tomako-broker.invalid")
        .header("x-unix-socket", socket_path.to_str().unwrap())
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(state);
    let response = timeout(
        Duration::from_secs(5),
        http_plain_proxy(
            /*policy_decider*/ None, /*environment_id*/ None, request,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let received = timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert!(received.starts_with("POST /capability?revision=1 HTTP/1.1\r\n"));
    assert!(!received.to_ascii_lowercase().contains("x-unix-socket"));
}

#[tokio::test]
async fn linux_socket_scope_rejects_sibling_and_swappable_alias_even_with_allow_all_flag() {
    let directory = tempfile::tempdir().unwrap();
    let own = directory.path().join("own.sock");
    let sibling = directory.path().join("sibling.sock");
    let alias = directory.path().join("alias.sock");
    let own_listener = UnixListener::bind(&own).unwrap();
    let sibling_listener = UnixListener::bind(&sibling).unwrap();
    std::os::unix::fs::symlink(&own, &alias).unwrap();
    let mut network = NetworkProxyConfig::default();
    network.set_allow_unix_sockets(vec![own.to_str().unwrap().to_string()]);
    network.dangerously_allow_all_unix_sockets = true;
    let state = Arc::new(network_proxy_state_for_policy(network));
    assert!(
        state
            .is_unix_socket_allowed(own.to_str().unwrap())
            .await
            .unwrap()
    );
    for path in [&sibling, &alias] {
        let mut request = Request::builder()
            .uri("http://tomako-broker.invalid/capability")
            .header("x-unix-socket", path.to_str().unwrap())
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(state.clone());
        let response = http_plain_proxy(
            /*policy_decider*/ None, /*environment_id*/ None, request,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert!(
        timeout(Duration::from_millis(20), own_listener.accept())
            .await
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(20), sibling_listener.accept())
            .await
            .is_err()
    );
}
