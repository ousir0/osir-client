//! 错误类型到 HTTP 状态码的映射
//!
//! 将 ProxyError 映射到合适的 HTTP 状态码，用于日志记录和手动构建错误响应

use super::ProxyError;

/// 将 ProxyError 映射到 HTTP 状态码
///
/// 映射规则：
/// - 上游错误：直接使用上游返回的状态码
/// - 超时：504 Gateway Timeout
/// - 连接失败：502 Bad Gateway
/// - 无可用 Provider：503 Service Unavailable
/// - 重试耗尽：503 Service Unavailable
/// - 认证错误：401 Unauthorized
/// - 配置/请求错误：400 Bad Request
/// - 转换错误：422 Unprocessable Entity
/// - 其他错误：500 Internal Server Error
pub fn map_proxy_error_to_status(error: &ProxyError) -> u16 {
    match error {
        // 服务状态错误：与 IntoResponse 保持一致
        ProxyError::AlreadyRunning => 409,
        ProxyError::NotRunning => 503,

        // 上游错误：使用实际状态码
        ProxyError::UpstreamError { status, .. } => *status,

        // 超时错误：504 Gateway Timeout
        ProxyError::Timeout(_) | ProxyError::StreamIdleTimeout(_) => 504,

        // 转发失败/连接失败：502 Bad Gateway
        ProxyError::ForwardFailed(_) => 502,

        // 无可用 Provider：503 Service Unavailable
        ProxyError::NoAvailableProvider => 503,

        // 所有供应商已熔断：503 Service Unavailable
        ProxyError::AllProvidersCircuitOpen => 503,

        // 未配置供应商：503 Service Unavailable
        ProxyError::NoProvidersConfigured => 503,

        // 重试耗尽：503 Service Unavailable
        ProxyError::MaxRetriesExceeded => 503,

        // Provider 不健康：503 Service Unavailable
        ProxyError::ProviderUnhealthy(_) => 503,

        // 配置错误/无效请求：400 Bad Request
        ProxyError::ConfigError(_) | ProxyError::InvalidRequest(_) => 400,

        // 认证错误：401 Unauthorized
        ProxyError::AuthError(_) => 401,

        // 数据库错误：500 Internal Server Error
        ProxyError::DatabaseError(_) => 500,

        // 转换错误：422 Unprocessable Entity
        ProxyError::TransformError(_) => 422,

        // 其他未知错误：500 Internal Server Error
        _ => 500,
    }
}

// These labels are the complete vocabulary allowed in newly persisted error details.
// Never derive a label from Display, an upstream body, a URL, or a JSON message.
const SAFE_ERROR_LABELS: &[&str] = &[
    "上游错误",
    "请求超时",
    "流式响应超时",
    "请求转发失败",
    "无可用供应商",
    "所有供应商已熔断",
    "未配置供应商",
    "重试次数已耗尽",
    "供应商不可用",
    "配置错误",
    "请求无效",
    "认证失败",
    "数据库错误",
    "请求或响应转换失败",
    "服务已在运行",
    "服务未运行",
    "监听失败",
    "停止超时",
    "停止失败",
    "响应超过大小上限",
    "内部错误",
    "请求失败",
];

fn safe_error_detail(label: &str, status: u16) -> String {
    // Do not turn an invalid/untrusted numeric status into arbitrary text.
    let status = if (100..=599).contains(&status) {
        status
    } else {
        502
    };
    format!("{label} (HTTP {status})")
}

/// Content-free error detail for logs, health records, diagnostics and IPC status.
/// Caller-facing protocol errors must continue using the original ProxyError.
pub fn get_safe_error_message(error: &ProxyError) -> String {
    let label = match error {
        ProxyError::UpstreamError { .. } => "上游错误",
        ProxyError::Timeout(_) => "请求超时",
        ProxyError::StreamIdleTimeout(_) => "流式响应超时",
        ProxyError::ForwardFailed(_) => "请求转发失败",
        ProxyError::NoAvailableProvider => "无可用供应商",
        ProxyError::AllProvidersCircuitOpen => "所有供应商已熔断",
        ProxyError::NoProvidersConfigured => "未配置供应商",
        ProxyError::MaxRetriesExceeded => "重试次数已耗尽",
        ProxyError::ProviderUnhealthy(_) => "供应商不可用",
        ProxyError::ConfigError(_) => "配置错误",
        ProxyError::InvalidRequest(_) => "请求无效",
        ProxyError::AuthError(_) => "认证失败",
        ProxyError::DatabaseError(_) => "数据库错误",
        ProxyError::TransformError(_) => "请求或响应转换失败",
        ProxyError::AlreadyRunning => "服务已在运行",
        ProxyError::NotRunning => "服务未运行",
        ProxyError::BindFailed(_) => "监听失败",
        ProxyError::StopTimeout => "停止超时",
        ProxyError::StopFailed(_) => "停止失败",
        ProxyError::ResponseBodyTooLarge(_) => "响应超过大小上限",
        ProxyError::Internal(_) => "内部错误",
    };
    safe_error_detail(label, map_proxy_error_to_status(error))
}

/// Final storage boundary for legacy/string-based usage writers (including streams).
/// Only exact, locally generated classifications survive. Everything else becomes
/// a generic classification; truncation and secret-pattern blacklists are unsafe.
pub fn normalize_persisted_error(status: u16, message: &str) -> String {
    SAFE_ERROR_LABELS
        .iter()
        .map(|label| safe_error_detail(label, status))
        .find(|safe| safe == message)
        .unwrap_or_else(|| safe_error_detail("请求失败", status))
}

/// Storage guard for health writers whose legacy signature has no status field.
/// Unknown text is replaced rather than inspected for patterns or truncated.
pub fn normalize_health_error(message: &str) -> String {
    if let Some((_, suffix)) = message.rsplit_once(" (HTTP ") {
        if let Some(status) = suffix.strip_suffix(')').and_then(|v| v.parse::<u16>().ok()) {
            if (100..=599).contains(&status)
                && normalize_persisted_error(status, message) == message
            {
                return message.to_string();
            }
        }
    }
    "请求失败".to_string()
}

/// Caller-facing message only: may contain upstream content. Never log or persist it.
/// 将 ProxyError 转换为用户友好的错误消息
pub fn get_error_message(error: &ProxyError) -> String {
    match error {
        ProxyError::UpstreamError { status, body } => {
            // 上游没给内容时要明说，否则用户会以为是 CC Switch 把错误内容吞了（#6895）
            let body = body
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("上游未返回错误内容");
            format!("上游错误 ({status}): {body}")
        }
        ProxyError::Timeout(msg) => format!("请求超时: {msg}"),
        ProxyError::ForwardFailed(msg) => format!("转发失败: {msg}"),
        ProxyError::NoAvailableProvider => "无可用 Provider".to_string(),
        ProxyError::AllProvidersCircuitOpen => "所有供应商已熔断，无可用渠道".to_string(),
        ProxyError::NoProvidersConfigured => "未配置供应商".to_string(),
        ProxyError::MaxRetriesExceeded => "所有 Provider 都失败，重试耗尽".to_string(),
        ProxyError::ProviderUnhealthy(msg) => format!("Provider 不健康: {msg}"),
        ProxyError::DatabaseError(msg) => format!("数据库错误: {msg}"),
        ProxyError::TransformError(msg) => format!("请求/响应转换错误: {msg}"),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a1_health_errors_accept_only_exact_controlled_classifications() {
        for raw in [
            "fixture-key-A1 fixture-prompt-A1",
            "upstream prose without recognizable secret patterns",
            "上游错误 (HTTP 429)\n",
            "上游错误 (HTTP 429) fixture-token-A1",
            "fixture-prompt-A1 (HTTP 429)",
            "上游错误 (HTTP 099)",
            "上游错误 (HTTP 65535)",
            "上游错误 (HTTP 0429)",
            "上游错误 (HTTP 429))",
        ] {
            assert_eq!(normalize_health_error(raw), "请求失败");
        }
        assert_eq!(
            normalize_health_error("上游错误 (HTTP 429)"),
            "上游错误 (HTTP 429)"
        );
    }

    use super::*;

    #[test]
    fn safe_errors_ignore_all_payloads_and_round_trip_storage_boundary() {
        let payload = "fixture-key-A1 fixture-token-A1 fixture-prompt-A1 fixture-image-A1";
        let errors = [
            ProxyError::UpstreamError {
                status: 429,
                body: Some(payload.into()),
            },
            ProxyError::Timeout(payload.into()),
            ProxyError::StreamIdleTimeout(30),
            ProxyError::ForwardFailed(payload.into()),
            ProxyError::NoAvailableProvider,
            ProxyError::AllProvidersCircuitOpen,
            ProxyError::NoProvidersConfigured,
            ProxyError::MaxRetriesExceeded,
            ProxyError::ProviderUnhealthy(payload.into()),
            ProxyError::ConfigError(payload.into()),
            ProxyError::InvalidRequest(payload.into()),
            ProxyError::AuthError(payload.into()),
            ProxyError::DatabaseError(payload.into()),
            ProxyError::TransformError(payload.into()),
            ProxyError::AlreadyRunning,
            ProxyError::NotRunning,
            ProxyError::BindFailed(payload.into()),
            ProxyError::StopTimeout,
            ProxyError::StopFailed(payload.into()),
            ProxyError::ResponseBodyTooLarge(1024),
            ProxyError::Internal(payload.into()),
        ];
        for error in errors {
            let safe = get_safe_error_message(&error);
            assert!(!safe.contains("fixture-"), "{safe}");
            assert_eq!(
                normalize_persisted_error(map_proxy_error_to_status(&error), &safe),
                safe
            );
        }
    }

    #[test]
    fn persisted_errors_accept_only_complete_controlled_labels() {
        for message in [
            "upstream returned arbitrary prose without a key prefix",
            "{\"error\":{\"message\":\"fixture-prompt-A1\"}}",
            "上游错误 (HTTP 500) fixture-token-A1",
            "上游错误 (HTTP 429)",
            "上游错误 (HTTP 500)\n",
            "",
        ] {
            assert_eq!(
                normalize_persisted_error(500, message),
                "请求失败 (HTTP 500)"
            );
        }
        let invalid = ProxyError::UpstreamError {
            status: 65535,
            body: None,
        };
        assert_eq!(get_safe_error_message(&invalid), "上游错误 (HTTP 502)");
    }

    #[tokio::test]
    async fn safe_internal_errors_preserve_caller_status_and_protocol_body() {
        use axum::response::IntoResponse;
        let payload = serde_json::json!({"error": {
            "type": "invalid_request_error", "code": "fixture_code",
            "message": "fixture-prompt-A1", "nested": {"key": "fixture-key-A1"}
        }});
        for status in [400, 401, 403, 429, 500, 503] {
            let error = ProxyError::UpstreamError {
                status,
                body: Some(payload.to_string()),
            };
            let safe = get_safe_error_message(&error);
            assert!(!safe.contains("fixture-"));
            assert_eq!(map_proxy_error_to_status(&error), status);
            let response = error.into_response();
            assert_eq!(response.status().as_u16(), status);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                payload
            );
        }
    }

    #[test]
    fn test_map_upstream_error() {
        let error = ProxyError::UpstreamError {
            status: 401,
            body: Some("Unauthorized".to_string()),
        };
        assert_eq!(map_proxy_error_to_status(&error), 401);
    }

    #[test]
    fn test_map_timeout_error() {
        let error = ProxyError::Timeout("Request timeout".to_string());
        assert_eq!(map_proxy_error_to_status(&error), 504);
    }

    #[test]
    fn test_map_connection_error() {
        let error = ProxyError::ForwardFailed("Connection refused".to_string());
        assert_eq!(map_proxy_error_to_status(&error), 502);
    }

    #[test]
    fn test_map_no_provider_error() {
        let error = ProxyError::NoAvailableProvider;
        assert_eq!(map_proxy_error_to_status(&error), 503);
    }

    #[test]
    fn test_map_status_matches_proxy_error_response_semantics() {
        assert_eq!(
            map_proxy_error_to_status(&ProxyError::AuthError("bad token".to_string())),
            401
        );
        assert_eq!(
            map_proxy_error_to_status(&ProxyError::ConfigError("bad config".to_string())),
            400
        );
        assert_eq!(
            map_proxy_error_to_status(&ProxyError::InvalidRequest("bad request".to_string())),
            400
        );
        assert_eq!(
            map_proxy_error_to_status(&ProxyError::TransformError("bad transform".to_string())),
            422
        );
        assert_eq!(
            map_proxy_error_to_status(&ProxyError::StreamIdleTimeout(30)),
            504
        );
    }

    #[test]
    fn test_get_error_message() {
        let error = ProxyError::UpstreamError {
            status: 500,
            body: Some("Internal Server Error".to_string()),
        };
        let msg = get_error_message(&error);
        assert!(msg.contains("上游错误"));
        assert!(msg.contains("500"));
        assert!(msg.contains("Internal Server Error"));
    }

    #[test]
    fn test_get_error_message_empty_body() {
        let error_empty = ProxyError::UpstreamError {
            status: 502,
            body: Some("   ".to_string()),
        };
        assert_eq!(
            get_error_message(&error_empty),
            "上游错误 (502): 上游未返回错误内容"
        );

        let error_none = ProxyError::UpstreamError {
            status: 502,
            body: None,
        };
        assert_eq!(
            get_error_message(&error_none),
            "上游错误 (502): 上游未返回错误内容"
        );
    }
}

#[cfg(test)]
pub(crate) mod test_log_capture {
    // Capture exactly the text delivered to file loggers, on this test's
    // runtime thread only. Other concurrent fixture logs are not collected.
    thread_local! {
        static A1_LOG_TEXT: std::cell::RefCell<Option<String>> = const {
            std::cell::RefCell::new(None)
        };
    }
    pub(crate) struct A1LogCapture;
    impl log::Log for A1LogCapture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            A1_LOG_TEXT.with(|slot| {
                if let Some(text) = slot.borrow_mut().as_mut() {
                    use std::fmt::Write;
                    writeln!(text, "{}", record.args()).unwrap();
                }
            });
        }
        fn flush(&self) {}
    }
    impl A1LogCapture {
        pub(crate) fn start() -> Self {
            static INIT: std::sync::Once = std::sync::Once::new();
            static LOGGER: A1LogCapture = A1LogCapture;
            INIT.call_once(|| {
                log::set_logger(&LOGGER).expect("fixture logger installs once");
                log::set_max_level(log::LevelFilter::Debug);
            });
            A1_LOG_TEXT.with(|slot| *slot.borrow_mut() = Some(String::new()));
            Self
        }
        pub(crate) fn text(&self) -> String {
            A1_LOG_TEXT.with(|slot| slot.borrow().as_ref().unwrap().clone())
        }
    }
    impl Drop for A1LogCapture {
        fn drop(&mut self) {
            A1_LOG_TEXT.with(|slot| *slot.borrow_mut() = None);
        }
    }
}
