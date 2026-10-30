//! Customer-service invoices, using the website's existing /tickets API only.
//!
//! IMPORTANT integration: load this file as a CHILD of services::osir::session
//! (#[path = "../../commands/osir_invoice.rs"] pub mod invoice;), then re-export
//! its commands from commands/mod.rs. This keeps access()/SecretRecord private
//! to the native session module; no new credential accessor or IPC token exists.
//! Contract source: desktop-invoice-scope/sub2api-new at 49fde4052be736fdf30d241ac8fa1a926818f5b2,
//! based on production baseline 76271c0f0e972d1766d9a67f57387c512934c44b.
//! Released on 2026-10-29. Never fall back to browser JWTs.
use super::{AuthManager, SessionError};
use crate::commands::OsirAccountState;
use crate::services::osir::request_budget::{AdmissionError, AttemptGuard, RequestClass};
use reqwest::header::{CONTENT_TYPE, RETRY_AFTER};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;

const PAGE_SIZE: u32 = 20;
const MAX_JSON: usize = 2 * 1024 * 1024;
const MAX_PDF: usize = 20 * 1024 * 1024;
const MAX_ORDERS: usize = 10_000;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct InvoiceError {
    code: &'static str,
}
fn fail(code: &'static str) -> InvoiceError {
    InvoiceError { code }
}
type Result<T> = std::result::Result<T, InvoiceError>;
fn session_error(error: SessionError) -> InvoiceError {
    fail(match error {
        SessionError::Superseded => "stale_session",
        SessionError::Unauthorized => "unauthorized",
        _ => "unavailable",
    })
}
#[derive(Serialize)]
pub struct InvoiceReply<T> {
    generation: u64,
    data: T,
}
fn reply<T>(generation: u64, data: T) -> InvoiceReply<T> {
    InvoiceReply { generation, data }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct InvoiceRecord {
    id: i64,
    ticket_no: String,
    #[serde(skip_serializing)]
    user_id: i64,
    #[serde(skip_serializing)]
    category: String,
    status: String,
    invoice_amount: f64,
    invoice_currency: String,
    invoice_order_count: usize,
    invoice_type: String,
    invoice_service_name: String,
    user_unread: bool,
    created_at: String,
    updated_at: String,
    resolved_at: Option<String>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct InvoiceOrder {
    id: i64,
    out_trade_no: String,
    payment_type: String,
    order_type: String,
    pay_amount: f64,
    currency: String,
    paid_at: Option<String>,
}
#[derive(Deserialize, Serialize)]
pub struct InvoiceEligibility {
    enabled: bool,
    eligible: bool,
    threshold: f64,
    amount: f64,
    currency: String,
    order_count: usize,
    #[serde(default, deserialize_with = "nullable_orders")]
    orders: Vec<InvoiceOrder>,
}
// The website's disabled eligibility response leaves its Go slice nil (JSON null).
fn nullable_orders<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<InvoiceOrder>, D::Error> {
    Option::<Vec<InvoiceOrder>>::deserialize(deserializer).map(Option::unwrap_or_default)
}
#[derive(Deserialize, Serialize)]
pub struct InvoiceAttachment {
    id: i64,
    #[serde(skip_serializing)]
    ticket_id: i64,
    #[serde(skip_serializing)]
    message_id: i64,
    file_name: String,
    content_type: String,
    size_bytes: i64,
}
#[derive(Deserialize, Serialize)]
pub struct InvoiceMessage {
    id: i64,
    #[serde(skip_serializing)]
    ticket_id: i64,
    sender_role: String,
    content: String,
    created_at: String,
    #[serde(default)]
    attachments: Vec<InvoiceAttachment>,
}
#[derive(Deserialize, Serialize)]
pub struct InvoiceDetail {
    ticket: InvoiceRecord,
    messages: Vec<InvoiceMessage>,
    invoice_orders: Vec<InvoiceOrder>,
}
#[derive(Deserialize, Serialize)]
pub struct InvoicePage {
    items: Vec<InvoiceRecord>,
    total: u64,
    page: u32,
    page_size: u32,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvoiceForm {
    title: String,
    tax_number: String,
    email: String,
    note: String,
}

fn field(value: &str, max: usize, multiline: bool) -> Result<String> {
    let value = value.trim();
    if value.chars().count() > max
        || value
            .chars()
            .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\r' | '\t')))
    {
        return Err(fail("invalid_input"));
    }
    Ok(value.to_owned())
}
impl InvoiceForm {
    /// Exact Chinese field labels from the website's NewTicketView + zh/misc.ts.
    fn content(&self) -> Result<String> {
        let title = field(&self.title, 120, false)?;
        let tax = field(&self.tax_number, 40, false)?;
        let email = field(&self.email, 255, false)?;
        let note = field(&self.note, 2000, true)?;
        let valid_email = email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !domain.contains('@')
        }) && !email.chars().any(char::is_whitespace);
        if title.is_empty() || tax.is_empty() || !valid_email {
            return Err(fail("invalid_input"));
        }
        Ok(format!(
            "发票抬头：{title}\n纳税人识别号：{tax}\n接收邮箱：{email}\n其他备注：{}",
            if note.is_empty() { "未填写" } else { &note }
        ))
    }
}
fn ids(orders: &[InvoiceOrder]) -> Result<BTreeSet<i64>> {
    let mut set = BTreeSet::new();
    if orders.len() > MAX_ORDERS {
        return Err(fail("invalid_response"));
    }
    for order in orders {
        if order.id <= 0
            || !set.insert(order.id)
            || !order.pay_amount.is_finite()
            || order.pay_amount <= 0.0
            || order.currency.len() > 12
            || order.out_trade_no.len() > 256
        {
            return Err(fail("invalid_response"));
        }
    }
    Ok(set)
}
impl InvoiceRecord {
    fn validate(&self, owner: i64) -> Result<()> {
        if self.id <= 0
            || self.user_id != owner
            || self.category != "invoice"
            || self.ticket_no.len() > 120
            || !self.invoice_amount.is_finite()
            || self.invoice_amount < 0.0
            || self.invoice_order_count > MAX_ORDERS
        {
            return Err(fail("invalid_response"));
        }
        Ok(())
    }
}
impl InvoiceDetail {
    fn validate(&self, owner: i64, id: Option<i64>) -> Result<()> {
        self.ticket.validate(owner)?;
        if id.is_some_and(|id| id != self.ticket.id)
            || self.messages.len() > 1000
            || self.invoice_orders.len() != self.ticket.invoice_order_count
        {
            return Err(fail("invalid_response"));
        }
        ids(&self.invoice_orders)?;
        let mut messages = BTreeSet::new();
        let mut attachments = BTreeSet::new();
        for message in &self.messages {
            if message.id <= 0
                || !messages.insert(message.id)
                || message.ticket_id != self.ticket.id
                || message.content.chars().count() > 5000
                || !matches!(message.sender_role.as_str(), "admin" | "user")
            {
                return Err(fail("invalid_response"));
            }
            for attachment in &message.attachments {
                if attachment.id <= 0
                    || !attachments.insert(attachment.id)
                    || attachment.ticket_id != self.ticket.id
                    || attachment.message_id != message.id
                    || attachment.file_name.chars().any(char::is_control)
                    || attachment.file_name.len() > 512
                    || attachment.content_type.len() > 128
                    || attachment.size_bytes < 0
                {
                    return Err(fail("invalid_response"));
                }
            }
        }
        Ok(())
    }
}
impl InvoiceEligibility {
    fn validate(&self) -> Result<()> {
        if !self.amount.is_finite()
            || self.amount < 0.0
            || !self.threshold.is_finite()
            || self.threshold < 0.0
            || self.orders.len() != self.order_count
        {
            return Err(fail("invalid_response"));
        }
        ids(&self.orders)?;
        Ok(())
    }
    fn select_all(&self, selected: &[i64]) -> Result<()> {
        self.validate()?;
        if !self.enabled {
            return Err(fail("disabled"));
        }
        if !self.eligible || self.orders.is_empty() {
            return Err(fail("not_eligible"));
        }
        let expected = ids(&self.orders)?;
        let actual: BTreeSet<_> = selected.iter().copied().collect();
        if actual.len() != selected.len() || actual != expected {
            return Err(fail("orders_changed"));
        }
        Ok(())
    }
}

enum Endpoint<'a> {
    List { page: u32, status: &'a str },
    Eligibility,
    Detail(i64),
    Create(&'a str),
    Pdf(i64, i64),
}
impl Endpoint<'_> {
    fn write(&self) -> bool {
        matches!(self, Self::Create(_))
    }
    fn path(&self) -> String {
        match self {
            Self::List { .. } => "tickets".into(),
            Self::Eligibility => "tickets/invoice/eligibility".into(),
            Self::Detail(id) => format!("tickets/{id}"),
            Self::Create(_) => "tickets/invoice".into(),
            Self::Pdf(id, attachment) => format!("tickets/{id}/attachments/{attachment}"),
        }
    }
}
struct WireResponse {
    status: u16,
    content_type: String,
    bytes: Vec<u8>,
    guard: AttemptGuard,
}
fn transport_error(write: bool) -> InvoiceError {
    fail(if write {
        "outcome_unknown"
    } else {
        "unavailable"
    })
}
fn admission(error: AdmissionError) -> InvoiceError {
    fail(match error {
        AdmissionError::Cooldown { .. } | AdmissionError::RateLimited { .. } => "rate_limited",
        _ => "unavailable",
    })
}
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if raw.len() > 128 {
        return None;
    }
    if let Ok(seconds) = raw.parse::<u64>() {
        return Some(Duration::from_secs(seconds.min(300)));
    }
    let date = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    Some(
        date.signed_duration_since(chrono::Utc::now())
            .to_std()
            .unwrap_or_default()
            .min(Duration::from_secs(300)),
    )
}
fn http_client() -> Result<&'static reqwest::Client> {
    static HTTP: OnceLock<std::result::Result<reqwest::Client, InvoiceError>> = OnceLock::new();
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| fail("unavailable"))
    })
    .as_ref()
    .map_err(Clone::clone)
}
fn endpoint_url(origin: &str, endpoint: &Endpoint<'_>) -> Result<url::Url> {
    let base = url::Url::parse(origin).map_err(|_| fail("unavailable"))?;
    if base.scheme() != "https"
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || base.path() != "/"
    {
        return Err(fail("unavailable"));
    }
    base.join(&format!("api/v1/{}", endpoint.path()))
        .map_err(|_| fail("unavailable"))
}
async fn send(auth: &AuthManager, endpoint: &Endpoint<'_>, token: &str) -> Result<WireResponse> {
    let url = endpoint_url(&auth.account_origin(), endpoint)?;
    let http = http_client()?;
    let mut builder = if endpoint.write() {
        http.post(url)
    } else {
        http.get(url)
    }
    .bearer_auth(token);
    match endpoint {
        Endpoint::List { page, status } => {
            builder = builder.query(&[
                ("category", "invoice".to_string()),
                ("page", page.to_string()),
                ("page_size", PAGE_SIZE.to_string()),
                ("status", status.to_string()),
            ]);
        }
        Endpoint::Create(content) => {
            builder = builder.json(&serde_json::json!({ "content": content }));
        }
        _ => {}
    }
    let request = builder.build().map_err(|_| fail("invalid_input"))?;
    let guard = auth
        .request_budget()
        .admit(RequestClass::Standard)
        .await
        .map_err(admission)?;
    let mut response = http
        .execute(request)
        .await
        .map_err(|_| transport_error(endpoint.write()))?;
    let status = response.status().as_u16();
    guard.observe_response(status, retry_after(response.headers()));
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let max = if matches!(endpoint, Endpoint::Pdf(..)) {
        MAX_PDF
    } else {
        MAX_JSON
    };
    let invalid = || {
        fail(if endpoint.write() {
            "outcome_unknown"
        } else {
            "invalid_response"
        })
    };
    if response
        .content_length()
        .is_some_and(|size| size > max as u64)
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| transport_error(endpoint.write()))?
    {
        if bytes.len().saturating_add(chunk.len()) > max {
            return Err(invalid());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(WireResponse {
        status,
        content_type,
        bytes,
        guard,
    })
}
fn response_error(status: u16, bytes: &[u8], write: bool) -> InvoiceError {
    #[derive(Deserialize)]
    struct ErrorEnvelope {
        #[serde(default)]
        reason: String,
    }
    let reason = serde_json::from_slice::<ErrorEnvelope>(bytes)
        .map(|e| e.reason)
        .unwrap_or_default();
    fail(match (status, reason.as_str()) {
        (401, _) => "unauthorized",
        (403, "SUPPORT_TICKET_INVOICE_DISABLED") => "disabled",
        (403, _) => "scope_required",
        (429, _) => "rate_limited",
        (400, "SUPPORT_TICKET_INVOICE_THRESHOLD") => "not_eligible",
        (409, "SUPPORT_TICKET_INVOICE_CONFLICT") => "orders_changed",
        (409, "SUPPORT_TICKET_OPEN_LIMIT") => "open_limit",
        (400, "SUPPORT_TICKET_CONTENT_INVALID") => "invalid_input",
        _ if write => "outcome_unknown",
        _ => "unavailable",
    })
}
fn decode<T: DeserializeOwned>(response: WireResponse, write: bool) -> Result<T> {
    if !(200..300).contains(&response.status) {
        return Err(response_error(response.status, &response.bytes, write));
    }
    #[derive(Deserialize)]
    struct Envelope<T> {
        code: i64,
        data: T,
    }
    let value: Envelope<T> = serde_json::from_slice(&response.bytes).map_err(|_| {
        fail(if write {
            "outcome_unknown"
        } else {
            "invalid_response"
        })
    })?;
    if value.code != 0 {
        return Err(transport_error(write));
    }
    response.guard.finish_success();
    Ok(value.data)
}
impl AuthManager {
    fn invoice_owner(&self, generation: u64) -> Result<i64> {
        let status = self.status();
        if !status.connected || status.generation != generation {
            return Err(fail("stale_session"));
        }
        status
            .profile
            .map(|p| p.id)
            .filter(|id| *id > 0)
            .ok_or_else(|| fail("stale_session"))
    }
    async fn invoice_wire(&self, generation: u64, endpoint: Endpoint<'_>) -> Result<WireResponse> {
        let owner = self.invoice_owner(generation)?;
        {
            let state = self.state.lock().map_err(|_| fail("stale_session"))?;
            if state.generation != generation {
                return Err(fail("stale_session"));
            }
            let scope = if endpoint.write() {
                "invoices:write"
            } else {
                "invoices:read"
            };
            if !state
                .record
                .as_ref()
                .is_some_and(|record| record.scope.split(' ').any(|item| item == scope))
            {
                return Err(fail("scope_required"));
            }
        }
        let (actual, record) = self.access().await.map_err(session_error)?;
        if actual != generation || record.account_id != owner.to_string() {
            return Err(fail("stale_session"));
        }
        let scope = if endpoint.write() {
            "invoices:write"
        } else {
            "invoices:read"
        };
        if !record.scope.split(' ').any(|item| item == scope) {
            return Err(fail("scope_required"));
        }
        let mut response = send(self, &endpoint, &record.access_token).await?;
        if response.status == 401 && !endpoint.write() {
            // GET only, exactly one coordinated refresh/replay, both attempts charged.
            drop(response);
            let (actual, next) = self
                .access_after_rejection(Some((generation, record)))
                .await
                .map_err(session_error)?;
            if actual != generation || next.account_id != owner.to_string() {
                return Err(fail("stale_session"));
            }
            if !next.scope.split(' ').any(|item| item == scope) {
                return Err(fail("scope_required"));
            }
            response = send(self, &endpoint, &next.access_token).await?;
        }
        if self.invoice_owner(generation)? != owner {
            return Err(fail("stale_session"));
        }
        Ok(response)
    }
    async fn invoice_json<T: DeserializeOwned>(
        &self,
        generation: u64,
        endpoint: Endpoint<'_>,
    ) -> Result<T> {
        let write = endpoint.write();
        decode(self.invoice_wire(generation, endpoint).await?, write)
    }
    async fn invoice_detail(&self, generation: u64, id: i64) -> Result<InvoiceDetail> {
        if id <= 0 {
            return Err(fail("invalid_input"));
        }
        let detail: InvoiceDetail = self.invoice_json(generation, Endpoint::Detail(id)).await?;
        detail.validate(self.invoice_owner(generation)?, Some(id))?;
        Ok(detail)
    }
    async fn invoice_page(&self, generation: u64, page: u32, status: &str) -> Result<InvoicePage> {
        if page == 0
            || page > 1_000_000
            || !matches!(status, "" | "pending_admin" | "pending_user" | "resolved")
        {
            return Err(fail("invalid_input"));
        }
        let result: InvoicePage = self
            .invoice_json(generation, Endpoint::List { page, status })
            .await?;
        if result.page != page
            || result.page_size != PAGE_SIZE
            || result.items.len() > PAGE_SIZE as usize
        {
            return Err(fail("invalid_response"));
        }
        let owner = self.invoice_owner(generation)?;
        let mut unique = BTreeSet::new();
        for ticket in &result.items {
            ticket.validate(owner)?;
            if !unique.insert(ticket.id) {
                return Err(fail("invalid_response"));
            }
        }
        Ok(result)
    }
}

// Only native operation receipts are persisted, never a second invoice data store.
// No form fields, content, account token, URL, balance or attachment bytes on disk.
#[derive(Deserialize, Serialize)]
struct Receipt {
    version: u8,
    attempt_id: String,
    order_ids: Vec<i64>,
    ticket_id: Option<i64>,
}
fn gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}
fn receipt_path(app: &tauri::AppHandle, auth: &AuthManager, generation: u64) -> Result<PathBuf> {
    let owner = auth.invoice_owner(generation)?;
    let digest = Sha256::digest(format!("{}\n{owner}", auth.account_origin()).as_bytes());
    let name: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|_| fail("unavailable"))?
        .join("invoice-operation-receipts")
        .join(format!("{name}.json")))
}
fn load_receipt(path: &Path) -> Result<Option<Receipt>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(fail("outcome_unknown")),
    };
    if !metadata.is_file() || metadata.len() > 256 * 1024 {
        return Err(fail("outcome_unknown"));
    }
    let receipt: Receipt =
        serde_json::from_slice(&std::fs::read(path).map_err(|_| fail("outcome_unknown"))?)
            .map_err(|_| fail("outcome_unknown"))?;
    if receipt.version != 1
        || uuid::Uuid::parse_str(&receipt.attempt_id).is_err()
        || receipt.order_ids.is_empty()
        || receipt.order_ids.len() > MAX_ORDERS
        || receipt.order_ids.iter().any(|id| *id <= 0)
        || receipt.order_ids.iter().collect::<BTreeSet<_>>().len() != receipt.order_ids.len()
        || receipt.ticket_id.is_some_and(|id| id <= 0)
    {
        return Err(fail("outcome_unknown"));
    }
    Ok(Some(receipt))
}
fn store_receipt(path: &Path, receipt: &Receipt) -> Result<()> {
    let directory = path.parent().ok_or_else(|| fail("outcome_unknown"))?;
    std::fs::create_dir_all(directory).map_err(|_| fail("outcome_unknown"))?;
    if !std::fs::symlink_metadata(directory)
        .map_err(|_| fail("outcome_unknown"))?
        .is_dir()
    {
        return Err(fail("outcome_unknown"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| fail("outcome_unknown"))?;
    }
    let mut file =
        tempfile::NamedTempFile::new_in(directory).map_err(|_| fail("outcome_unknown"))?;
    serde_json::to_writer(&mut file, receipt).map_err(|_| fail("outcome_unknown"))?;
    file.as_file()
        .sync_all()
        .map_err(|_| fail("outcome_unknown"))?;
    file.persist(path).map_err(|_| fail("outcome_unknown"))?;
    #[cfg(unix)]
    std::fs::File::open(directory)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| fail("outcome_unknown"))?;
    Ok(())
}
fn definitely_rejected(error: &InvoiceError) -> bool {
    matches!(
        error.code,
        "scope_required"
            | "unauthorized"
            | "rate_limited"
            | "invalid_input"
            | "disabled"
            | "not_eligible"
            | "orders_changed"
            | "open_limit"
            | "unavailable"
    )
}

#[derive(Serialize)]
pub struct InvoiceAttempt {
    state: &'static str,
    ticket_id: Option<i64>,
}
#[tauri::command]
pub async fn osir_invoice_list(
    account: State<'_, OsirAccountState>,
    generation: u64,
    page: u32,
    status: String,
) -> Result<InvoiceReply<InvoicePage>> {
    Ok(reply(
        generation,
        account.auth.invoice_page(generation, page, &status).await?,
    ))
}
#[tauri::command]
pub async fn osir_invoice_eligibility(
    account: State<'_, OsirAccountState>,
    generation: u64,
) -> Result<InvoiceReply<InvoiceEligibility>> {
    let eligibility: InvoiceEligibility = account
        .auth
        .invoice_json(generation, Endpoint::Eligibility)
        .await?;
    eligibility.validate()?;
    Ok(reply(generation, eligibility))
}
#[tauri::command]
pub async fn osir_invoice_detail(
    account: State<'_, OsirAccountState>,
    generation: u64,
    id: i64,
) -> Result<InvoiceReply<InvoiceDetail>> {
    Ok(reply(
        generation,
        account.auth.invoice_detail(generation, id).await?,
    ))
}

/// Read-only reconciliation; never calls POST, never clears an unknown attempt
/// merely because a list page is empty, old, filtered, unavailable, or incomplete.
#[tauri::command]
pub async fn osir_invoice_attempt(
    app: tauri::AppHandle,
    account: State<'_, OsirAccountState>,
    generation: u64,
) -> Result<InvoiceReply<InvoiceAttempt>> {
    let _lock = gate().lock().await;
    let path = receipt_path(&app, &account.auth, generation)?;
    let Some(mut receipt) = load_receipt(&path)? else {
        return Ok(reply(
            generation,
            InvoiceAttempt {
                state: "none",
                ticket_id: None,
            },
        ));
    };
    if receipt.ticket_id.is_none() {
        let wanted: BTreeSet<_> = receipt.order_ids.iter().copied().collect();
        // Bounded reads: no polling or unbounded scan against account budgets.
        let mut inspected = 0;
        'pages: for page in 1..=5 {
            let list = account.auth.invoice_page(generation, page, "").await?;
            for ticket in list.items {
                if ticket.invoice_order_count < wanted.len() {
                    continue;
                }
                if inspected >= 10 {
                    break 'pages;
                }
                inspected += 1;
                let detail = account.auth.invoice_detail(generation, ticket.id).await?;
                if wanted.is_subset(&ids(&detail.invoice_orders)?) {
                    receipt.ticket_id = Some(ticket.id);
                    store_receipt(&path, &receipt)?;
                    break 'pages;
                }
            }
            if u64::from(page * PAGE_SIZE) >= list.total {
                break;
            }
        }
    }
    account.auth.invoice_owner(generation)?;
    Ok(reply(
        generation,
        InvoiceAttempt {
            state: if receipt.ticket_id.is_some() {
                "confirmed"
            } else {
                "unknown"
            },
            ticket_id: receipt.ticket_id,
        },
    ))
}

#[tauri::command]
pub async fn osir_invoice_apply(
    app: tauri::AppHandle,
    account: State<'_, OsirAccountState>,
    generation: u64,
    attempt_id: String,
    form: InvoiceForm,
    order_ids: Vec<i64>,
) -> Result<InvoiceReply<InvoiceDetail>> {
    if uuid::Uuid::parse_str(&attempt_id).is_err()
        || order_ids.is_empty()
        || order_ids.len() > MAX_ORDERS
    {
        return Err(fail("invalid_input"));
    }
    let content = form.content()?;
    let _lock = gate().lock().await;
    let path = receipt_path(&app, &account.auth, generation)?;
    if let Some(previous) = load_receipt(&path)? {
        match previous.ticket_id {
            None => return Err(fail("outcome_unknown")),
            Some(id) if previous.attempt_id == attempt_id => {
                return Ok(reply(
                    generation,
                    account.auth.invoice_detail(generation, id).await?,
                ))
            }
            _ => {}
        }
    }
    let eligibility: InvoiceEligibility = account
        .auth
        .invoice_json(generation, Endpoint::Eligibility)
        .await?;
    eligibility.select_all(&order_ids)?;
    account.auth.invoice_owner(generation)?;
    let mut receipt = Receipt {
        version: 1,
        attempt_id,
        order_ids,
        ticket_id: None,
    };
    // Durable tombstone BEFORE a possibly committed request. No HTTP idempotency
    // header/field is invented. Crash/cancellation remains unknown across restart.
    store_receipt(&path, &receipt)?;
    let result: Result<InvoiceDetail> = account
        .auth
        .invoice_json(generation, Endpoint::Create(&content))
        .await;
    let detail = match result {
        Ok(detail) => detail,
        Err(error) => {
            if definitely_rejected(&error) {
                std::fs::remove_file(&path).map_err(|_| fail("outcome_unknown"))?;
            }
            return Err(error);
        }
    };
    detail
        .validate(account.auth.invoice_owner(generation)?, None)
        .map_err(|_| fail("outcome_unknown"))?;
    if !receipt
        .order_ids
        .iter()
        .all(|id| detail.invoice_orders.iter().any(|order| order.id == *id))
    {
        return Err(fail("outcome_unknown"));
    }
    receipt.ticket_id = Some(detail.ticket.id);
    store_receipt(&path, &receipt)?;
    Ok(reply(generation, detail))
}

fn pdf_bytes(response: WireResponse, expected_size: i64) -> Result<Vec<u8>> {
    if !(200..300).contains(&response.status) {
        return Err(response_error(response.status, &response.bytes, false));
    }
    if response.content_type != "application/pdf"
        || expected_size <= 0
        || expected_size > MAX_PDF as i64
        || response.bytes.len() as i64 != expected_size
        || !response.bytes.starts_with(b"%PDF-")
    {
        return Err(fail("pdf_unavailable"));
    }
    response.guard.finish_success();
    Ok(response.bytes)
}
fn save_pdf(
    path: &Path,
    bytes: &[u8],
    auth: &AuthManager,
    generation: u64,
    owner: i64,
) -> Result<()> {
    if !path.is_absolute()
        || path
            .extension()
            .and_then(|e| e.to_str())
            .is_none_or(|e| !e.eq_ignore_ascii_case("pdf"))
    {
        return Err(fail("save_failed"));
    }
    let parent = path.parent().ok_or_else(|| fail("save_failed"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|_| fail("save_failed"))?;
    file.write_all(bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|_| fail("save_failed"))?;
    // Linearize the final commit with logout/account switching. No credential I/O.
    let state = auth.state.lock().map_err(|_| fail("stale_session"))?;
    if state.generation != generation
        || state
            .record
            .as_ref()
            .is_none_or(|r| r.account_id != owner.to_string())
    {
        return Err(fail("stale_session"));
    }
    file.persist_noclobber(path)
        .map_err(|_| fail("save_failed"))?;
    drop(state);
    Ok(())
}
#[derive(Serialize)]
pub struct InvoiceDownload {
    saved: bool,
}
/// No URL/path accepted from React. No browser token, blob URL, shell opener or
/// remote filename; same-origin authenticated bytes and a native save dialog.
#[tauri::command]
pub async fn osir_invoice_download_pdf(
    app: tauri::AppHandle,
    account: State<'_, OsirAccountState>,
    generation: u64,
    id: i64,
    attachment_id: i64,
) -> Result<InvoiceReply<InvoiceDownload>> {
    let owner = account.auth.invoice_owner(generation)?;
    let detail = account.auth.invoice_detail(generation, id).await?;
    let attachment = detail
        .messages
        .iter()
        .filter(|m| m.sender_role == "admin")
        .flat_map(|m| &m.attachments)
        .find(|a| {
            a.id == attachment_id
                && a.content_type.eq_ignore_ascii_case("application/pdf")
                && a.size_bytes > 0
                && a.size_bytes <= MAX_PDF as i64
        })
        .ok_or_else(|| fail("pdf_unavailable"))?;
    let size = attachment.size_bytes;
    let destination = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("osir-invoice-{id}-{attachment_id}.pdf"))
            .blocking_save_file()
    })
    .await
    .map_err(|_| fail("save_failed"))?;
    account.auth.invoice_owner(generation)?;
    let Some(destination) = destination else {
        return Ok(reply(generation, InvoiceDownload { saved: false }));
    };
    let path = destination.into_path().map_err(|_| fail("save_failed"))?;
    let response = account
        .auth
        .invoice_wire(generation, Endpoint::Pdf(id, attachment_id))
        .await?;
    let bytes = pdf_bytes(response, size)?;
    let auth = account.auth.clone();
    tauri::async_runtime::spawn_blocking(move || save_pdf(&path, &bytes, &auth, generation, owner))
        .await
        .map_err(|_| fail("save_failed"))??;
    Ok(reply(generation, InvoiceDownload { saved: true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn order(id: i64) -> InvoiceOrder {
        InvoiceOrder {
            id,
            out_trade_no: format!("FIXTURE-{id}"),
            payment_type: "fixture".into(),
            order_type: "balance".into(),
            pay_amount: 600.0,
            currency: "CNY".into(),
            paid_at: None,
        }
    }
    fn eligibility() -> InvoiceEligibility {
        InvoiceEligibility {
            enabled: true,
            eligible: true,
            threshold: 500.0,
            amount: 1200.0,
            currency: "CNY".into(),
            order_count: 2,
            orders: vec![order(1), order(2)],
        }
    }
    fn wire(status: u16, content_type: &str, bytes: &[u8]) -> WireResponse {
        WireResponse {
            status,
            content_type: content_type.into(),
            bytes: bytes.to_vec(),
            guard: crate::services::osir::request_budget::RequestBudget::default()
                .try_admit(RequestClass::Standard)
                .unwrap(),
        }
    }
    #[test]
    fn website_content_contract_and_validation() {
        let form = InvoiceForm {
            title: " 示例抬头 ".into(),
            tax_number: " FIXTURE-TAX-ID ".into(),
            email: " invoice@example.test ".into(),
            note: "".into(),
        };
        assert_eq!(form.content().unwrap(), "发票抬头：示例抬头\n纳税人识别号：FIXTURE-TAX-ID\n接收邮箱：invoice@example.test\n其他备注：未填写");
        for tax in ["", "   ", "x\n接收邮箱：override"] {
            assert!(InvoiceForm {
                tax_number: tax.into(),
                title: form.title.clone(),
                email: form.email.clone(),
                note: form.note.clone(),
            }
            .content()
            .is_err());
        }
        assert!(InvoiceForm {
            title: "x\n接收邮箱：override".into(),
            ..form
        }
        .content()
        .is_err());
    }
    #[test]
    fn disabled_website_eligibility_accepts_null_orders() {
        let value: InvoiceEligibility = serde_json::from_value(serde_json::json!({
            "enabled":false,"eligible":false,"threshold":500,"amount":0,
            "currency":"CNY","order_count":0,"orders":null
        }))
        .unwrap();
        assert!(value.validate().is_ok());
        assert_eq!(value.select_all(&[]).unwrap_err().code, "disabled");
    }
    #[test]
    fn subset_duplicate_stale_or_disabled_selection_is_rejected() {
        assert!(eligibility().select_all(&[2, 1]).is_ok());
        for ids in [vec![1], vec![1, 1], vec![1, 3]] {
            assert_eq!(
                eligibility().select_all(&ids).unwrap_err().code,
                "orders_changed"
            );
        }
        let mut value = eligibility();
        value.enabled = false;
        assert_eq!(value.select_all(&[1, 2]).unwrap_err().code, "disabled");
    }
    #[test]
    fn fixed_routes_reject_origins_and_never_accept_remote_urls() {
        let endpoint = Endpoint::Pdf(10, 21);
        assert_eq!(
            endpoint_url("https://account.example.test", &endpoint)
                .unwrap()
                .as_str(),
            "https://account.example.test/api/v1/tickets/10/attachments/21"
        );
        for origin in [
            "http://example.test",
            "https://secret@example.test",
            "https://example.test?token=secret",
            "https://example.test/else/",
        ] {
            assert!(endpoint_url(origin, &endpoint).is_err());
        }
        assert!(Endpoint::Create("body").write());
        assert!(!endpoint.write());
    }
    #[test]
    fn errors_are_allowlisted_and_uncertain_posts_never_become_retryable() {
        assert_eq!(
            response_error(
                403,
                br#"{"reason":"INSUFFICIENT_SCOPE","message":"Bearer SECRET"}"#,
                false
            )
            .code,
            "scope_required"
        );
        assert_eq!(response_error(503, b"SECRET", true).code, "outcome_unknown");
        assert!(!definitely_rejected(&transport_error(true)));
        assert!(!definitely_rejected(&fail("stale_session")));
        assert!(
            !serde_json::to_string(&response_error(500, b"SECRET", false))
                .unwrap()
                .contains("SECRET")
        );
    }
    #[test]
    fn pdf_requires_metadata_content_type_magic_and_bounded_exact_length() {
        let bytes = b"%PDF-1.7\nfixture";
        assert!(pdf_bytes(wire(200, "application/pdf", bytes), bytes.len() as i64).is_ok());
        for (mime, data, size) in [
            ("text/html", bytes.as_slice(), bytes.len() as i64),
            ("application/pdf", b"<html>".as_slice(), 6),
            ("application/pdf", bytes.as_slice(), 1),
            ("application/pdf", bytes.as_slice(), MAX_PDF as i64 + 1),
        ] {
            assert!(pdf_bytes(wire(200, mime, data), size).is_err());
        }
    }
    #[test]
    fn durable_receipt_survives_restart_without_form_or_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipts").join("fixture.json");
        let mut receipt = Receipt {
            version: 1,
            attempt_id: uuid::Uuid::new_v4().to_string(),
            order_ids: vec![1, 2],
            ticket_id: None,
        };
        store_receipt(&path, &receipt).unwrap();
        assert!(load_receipt(&path).unwrap().unwrap().ticket_id.is_none());
        receipt.ticket_id = Some(9);
        store_receipt(&path, &receipt).unwrap();
        assert_eq!(load_receipt(&path).unwrap().unwrap().ticket_id, Some(9));
        let json = std::fs::read_to_string(&path).unwrap();
        for field in ["email", "content", "access_token", "title"] {
            assert!(!json.contains(field));
        }
        std::fs::write(path, "truncated").unwrap();
        assert_eq!(
            load_receipt(&directory.path().join("receipts/fixture.json"))
                .err()
                .unwrap()
                .code,
            "outcome_unknown"
        );
    }
    #[test]
    fn strict_envelope_and_projection_drop_account_keys_and_raw_relations() {
        let value = serde_json::json!({"id":1,"user_id":7,"category":"invoice","ticket_no":"INV-fixture","status":"resolved","invoice_amount":600,"invoice_currency":"CNY","invoice_order_count":1,"invoice_type":"vat_ordinary","invoice_service_name":"technical_service","user_unread":false,"created_at":"2026-10-28T00:00:00Z","updated_at":"2026-10-28T00:00:00Z","user":{"api_key":"SECRET"},"access_token":"SECRET"});
        let record: InvoiceRecord = serde_json::from_value(value).unwrap();
        assert!(record.validate(7).is_ok());
        assert!(record.validate(8).is_err());
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("SECRET"));
        assert!(!json.contains("user_id"));
        assert!(decode::<InvoicePage>(wire(200, "application/json", b"{}"), false).is_err());
        assert_eq!(
            decode::<InvoicePage>(wire(201, "application/json", b"{}"), true)
                .err()
                .unwrap()
                .code,
            "outcome_unknown"
        );
    }
}
