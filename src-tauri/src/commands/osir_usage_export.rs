//! Fixed billing columns only: never serialize arbitrary wire dictionaries.
use serde_json::Value;

const COLUMNS: &[(&str, &str)] = &[
    ("时间", "created_at"),
    ("模型", "model"),
    ("分组 ID", "group_id"),
    ("密钥 ID", "api_key_id"),
    ("计费来源（0余额/1订阅）", "billing_type"),
    ("计费模式", "billing_mode"),
    ("请求类型", "request_type"),
    ("输入 Token", "input_tokens"),
    ("输出 Token", "output_tokens"),
    ("缓存写入 Token", "cache_creation_tokens"),
    ("缓存读取 Token", "cache_read_tokens"),
    ("图片数量", "image_count"),
    ("标准费用 USD", "total_cost"),
    ("实际费用 USD", "actual_cost"),
    ("请求 ID", "request_id"),
];

fn cell(value: &Value) -> String {
    let text = match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        _ => String::new(),
    };
    // CSV quoting alone does not prevent spreadsheet formula execution.
    let unsafe_prefix = matches!(
        text.trim_start().chars().next(),
        Some('=' | '+' | '-' | '@')
    ) || text.starts_with(['\t', '\r', '\n']);
    format!(
        "\"{}{}\"",
        if unsafe_prefix { "'" } else { "" },
        text.replace('\"', "\"\"")
    )
}

pub(super) fn encode(data: Value) -> Result<(String, usize), String> {
    let items = data
        .get("items")
        .and_then(Value::as_array)
        .filter(|items| items.len() <= 100)
        .ok_or("当前页数据不可导出")?;
    if items.is_empty() {
        return Err("当前页没有可导出的记录".into());
    }
    let mut csv = String::from("\u{feff}");
    csv.push_str(
        &COLUMNS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(","),
    );
    csv.push_str("\r\n");
    for item in items {
        csv.push_str(
            &COLUMNS
                .iter()
                .map(|(_, key)| cell(&item[*key]))
                .collect::<Vec<_>>()
                .join(","),
        );
        csv.push_str("\r\n");
    }
    if csv.len() > 1_048_576 {
        return Err("当前页导出内容超出大小限制".into());
    }
    Ok((csv, items.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn export_is_bounded_and_uses_fixed_safe_columns() {
        let (csv, count) = encode(json!({"items":[{"model":"=HYPERLINK(secret)","request_id":"a,\"b\n","actual_cost":0.3,"key":"NEVER_EXPORT","prompt":"NEVER_EXPORT"}]})).unwrap();
        assert_eq!(count, 1);
        assert!(csv.contains("\"'=HYPERLINK(secret)\""));
        assert!(csv.contains("\"a,\"\"b\n\""));
        assert!(!csv.contains("NEVER_EXPORT"));
        assert!(encode(json!({"items":[]})).is_err());
        assert!(encode(json!({"items": vec![json!({});101]})).is_err());
    }
}
