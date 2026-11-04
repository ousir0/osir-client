use super::super::{
    content_encoding::{decompress_body_with_limit, get_content_encoding, DecompressError},
    ProxyError,
};
use axum::{body::Body, http::HeaderMap};
use bytes::Bytes;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use std::time::Duration;

pub(crate) const MAX_REQUEST_BODY_BYTES: usize = 200 * 1024 * 1024;
const REQUEST_BODY_TIMEOUT: Duration = Duration::from_secs(120);

fn request_error(status: u16, message: String) -> ProxyError {
    ProxyError::UpstreamError {
        status,
        body: Some(
            serde_json::json!({"error": {"type": "request_body_error", "message": message}})
                .to_string(),
        ),
    }
}

pub(crate) async fn read_request_body(body: Body) -> Result<Bytes, ProxyError> {
    read_with_limit(body, MAX_REQUEST_BODY_BYTES, REQUEST_BODY_TIMEOUT).await
}

async fn read_with_limit(body: Body, limit: usize, timeout: Duration) -> Result<Bytes, ProxyError> {
    let collected = tokio::time::timeout(timeout, Limited::new(body, limit).collect())
        .await
        .map_err(|_| request_error(408, "Request body read timed out".into()))?;
    match collected {
        Ok(body) => Ok(body.to_bytes()),
        Err(error) if error.is::<LengthLimitError>() => Err(request_error(
            413,
            format!("Request body exceeds {limit} bytes before decompression"),
        )),
        Err(_) => Err(request_error(400, "Failed to read request body".into())),
    }
}

pub(crate) fn decode_request_body(
    headers: &mut HeaderMap,
    body: Bytes,
) -> Result<Bytes, ProxyError> {
    decode_with_limit(headers, body, MAX_REQUEST_BODY_BYTES)
}

fn decode_with_limit(
    headers: &mut HeaderMap,
    body: Bytes,
    limit: usize,
) -> Result<Bytes, ProxyError> {
    let Some(encoding) = get_content_encoding(headers) else {
        return Ok(body);
    };
    let decoded = match decompress_body_with_limit(&encoding, &body, limit) {
        Ok(Some(decoded)) => decoded,
        Ok(None) => {
            return Err(ProxyError::InvalidRequest(
                "Unsupported request content-encoding".into(),
            ))
        }
        Err(DecompressError::TooLarge { .. }) => {
            return Err(request_error(
                413,
                format!("Request body exceeds {limit} bytes after decompression"),
            ))
        }
        Err(DecompressError::Io(_)) => {
            return Err(ProxyError::InvalidRequest(
                "Failed to decompress request body".into(),
            ))
        }
    };
    headers.remove(axum::http::header::CONTENT_ENCODING);
    headers.remove(axum::http::header::CONTENT_LENGTH);
    headers.remove(axum::http::header::TRANSFER_ENCODING);
    Ok(Bytes::from(decoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::CONTENT_ENCODING;
    use std::io::Write;

    fn status(error: ProxyError) -> u16 {
        match error {
            ProxyError::UpstreamError { status, .. } => status,
            other => panic!("unexpected: {other}"),
        }
    }

    #[tokio::test]
    async fn request_body_limits_chunked_and_fixed_bodies() {
        let chunks = futures::stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::from_static(b"ab")),
            Ok(Bytes::from_static(b"cd")),
        ]);
        assert_eq!(
            read_with_limit(Body::from_stream(chunks), 4, Duration::from_secs(1))
                .await
                .unwrap(),
            "abcd"
        );
        let chunks = futures::stream::iter(vec![
            Ok::<_, std::io::Error>(Bytes::from_static(b"ab")),
            Ok(Bytes::from_static(b"cde")),
        ]);
        assert_eq!(
            status(
                read_with_limit(Body::from_stream(chunks), 4, Duration::from_secs(1))
                    .await
                    .unwrap_err()
            ),
            413
        );
        assert_eq!(
            status(
                read_with_limit(Body::from("abcde"), 4, Duration::from_secs(1))
                    .await
                    .unwrap_err()
            ),
            413
        );
    }

    #[tokio::test]
    async fn request_body_read_timeout_is_distinct() {
        let pending = futures::stream::pending::<Result<Bytes, std::io::Error>>();
        assert_eq!(
            status(
                read_with_limit(Body::from_stream(pending), 4, Duration::from_millis(10))
                    .await
                    .unwrap_err()
            ),
            408
        );
    }

    #[test]
    fn request_body_decoding_enforces_limit_and_preserves_headers_on_failure() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"abcde").unwrap();
        let compressed = Bytes::from(encoder.finish().unwrap());
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_ENCODING, "gzip".parse().unwrap());
        assert_eq!(
            status(decode_with_limit(&mut headers, compressed.clone(), 4).unwrap_err()),
            413
        );
        assert!(headers.contains_key(CONTENT_ENCODING));
        assert_eq!(
            decode_with_limit(&mut headers, compressed, 5).unwrap(),
            "abcde"
        );
        assert!(!headers.contains_key(CONTENT_ENCODING));
    }
}
