//! Chat attachments (t3code `AttachmentUpload`, `attachmentStore`,
//! `persistChatAttachments`): signed one-time upload URLs, the upload route's
//! storage, pending-upload deletion, and inline data-URL persistence.
//!
//! Files live in `<userdata>/attachments/<attachmentId><ext>`. Upload tokens
//! are `<base64url claims>.<base64url HMAC-SHA256>`, keyed by a random secret
//! kept in `<userdata>/attachment-signing-key`.

use crate::rpc::shell::{RecordOrNot, count_or_zero, if_fits, list_if_list, text_or_empty};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hmac::{KeyInit, Mac};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use domovoi_model::typed::{AssetCreateUrlResult, ChatAttachment, ImageAttachment, ImageSource, PersistChatAttachmentsResult};

pub const UPLOAD_ROUTE_PREFIX: &str = "/api/attachments/upload";
const UPLOAD_URL_TTL_MS: i64 = 10 * 60_000;
const PENDING: &str = "pending";
const THREAD_SEGMENT_MAX_CHARS: usize = 80;

type HmacSha256 = hmac::Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Claims {
    pub version: u8,
    pub kind: String,
    #[serde(rename = "type")]
    pub attachment_type: String,
    pub attachment_id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub expires_at: i64,
}

/// What the token of an attachment's URL says: which attachment may be read, and until when.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ReadClaims {
    version: u8,
    kind: String,
    attachment_id: String,
    expires_at: i64,
}

/// A failed call about attachments, written as its tagged error.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum Failure {
    /// The key upload URLs are signed with could not be read or made (`AttachmentUploadSigningKeyError`).
    #[error("the attachment upload signing key could not be loaded: {cause}")]
    #[serde(rename = "AttachmentUploadSigningKeyError")]
    SigningKey { cause: String },

    /// No URL was made for an attachment. t3code has no error of this name:
    /// its `assets.createUrl` fails with one of `AssetAccessError`.
    #[error("{message}")]
    #[serde(rename = "AssetUrlError")]
    AssetUrl { message: String },

    #[error("{message}")]
    #[serde(rename = "PersistChatAttachmentsError")]
    Persist { message: String },
}

pub fn dir(userdata: &Path) -> PathBuf {
    userdata.join("attachments")
}

fn signing_key(userdata: &Path) -> std::io::Result<Vec<u8>> {
    static LOCK: parking_lot::Mutex<()> = parking_lot::const_mutex(());
    let _guard = LOCK.lock();
    let path = userdata.join("attachment-signing-key");
    match std::fs::read(&path) {
        Ok(k) if k.len() >= 32 => Ok(k),
        _ => {
            let key = crate::util::random_bytes::<32>().to_vec();
            std::fs::write(&path, &key)?;
            Ok(key)
        }
    }
}

fn sign(key: &[u8], payload: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("any key length");
    mac.update(payload.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

pub(crate) fn sign_asset_claims(userdata: &Path, claims: &impl Serialize) -> std::io::Result<String> {
    let key = signing_key(userdata)?;
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims)?);
    Ok(format!("{payload}.{}", sign(&key, &payload)))
}

pub(crate) fn verify_asset_claims<T: serde::de::DeserializeOwned>(userdata: &Path, token: &str) -> Option<T> {
    let (payload, signature) = token.split_once('.')?;
    let key = std::fs::read(userdata.join("attachment-signing-key")).ok()?;
    let mut mac = HmacSha256::new_from_slice(&key).ok()?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&URL_SAFE_NO_PAD.decode(signature).ok()?).ok()?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()
}

/// `attachmentFileExtension`: a short lowercase extension, else `.bin`.
pub fn file_extension(name: &str) -> String {
    let ext = Path::new(name).extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
    let ok = ext.len() >= 2 && ext.len() <= 11 && ext[1..].chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if ext == ".part" || !ok { ".bin".into() } else { ext }
}

/// `inferImageExtension` for the four uploadable image types.
pub fn image_extension(mime_type: &str, name: &str) -> String {
    match mime_type.to_lowercase().as_str() {
        "image/png" => ".png".into(),
        "image/jpeg" | "image/jpg" => ".jpg".into(),
        "image/gif" => ".gif".into(),
        "image/webp" => ".webp".into(),
        "image/svg+xml" => ".svg".into(),
        _ => {
            let ext = file_extension(name);
            if ext == ".bin" { ".png".into() } else { ext }
        }
    }
}

/// `attachmentIdExtensionSuffix`.
fn id_suffix(extension: Option<&str>) -> String {
    match extension.map(|e| e.trim_start_matches('.').to_lowercase()) {
        None => String::new(),
        Some(e) if !e.is_empty() && e.len() <= 10 && e.chars().all(|c| c.is_ascii_alphanumeric()) => format!("-{e}"),
        Some(_) => "-bin".into(),
    }
}

/// `toSafeThreadAttachmentSegment`.
pub fn thread_segment(thread_id: &str) -> Option<String> {
    let mut s = String::new();
    for c in thread_id.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            s.push(c);
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let collapsed = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    let mut seg: String = collapsed.trim_matches(|c| c == '-' || c == '_').chars().take(THREAD_SEGMENT_MAX_CHARS).collect();
    while seg.ends_with(['-', '_']) {
        seg.pop();
    }
    match seg.as_str() {
        "" => None,
        PENDING => Some("_pending".into()),
        _ => Some(seg),
    }
}

/// `createDeterministicAttachmentId`: same thread, message, index and bytes give the same id.
pub fn deterministic_id(thread_id: &str, stable_key: &str) -> Option<String> {
    let seg = thread_segment(thread_id)?;
    let hashed = serde_json::to_string(&[thread_id, stable_key]).expect("plain data serializes");
    let hash = hex(&Sha256::digest(hashed.as_bytes()))[..32].to_owned();
    Some(format!("{seg}-{}-{}-{}-{}-{}", &hash[..8], &hash[8..12], &hash[12..16], &hash[16..20], &hash[20..]))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn is_svg(mime: &str, name: &str) -> bool {
    mime.eq_ignore_ascii_case("image/svg+xml") || name.to_lowercase().ends_with(".svg")
}

/// What an upload URL is asked for (`AttachmentCreateUploadUrlInput`). Every
/// field has a fallback, taken too when the field is not of its type: an
/// image, named `attachment`, of no type and no size.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UploadRequest {
    /// `file` for a file; anything else is stored as an image is.
    #[serde(default, rename = "type", deserialize_with = "if_fits")]
    attachment_type: Option<String>,
    #[serde(default, deserialize_with = "if_fits")]
    name: Option<String>,
    #[serde(default, deserialize_with = "text_or_empty")]
    mime_type: String,
    #[serde(default, deserialize_with = "count_or_zero")]
    size_bytes: u64,
}

/// Where to send an attachment's bytes (`AttachmentCreateUploadUrlResult`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadUrl {
    pub attachment_id: String,
    /// Relative to the server's origin; its last part is the signed token.
    pub relative_url: String,
    /// In milliseconds since the Unix epoch.
    pub expires_at: i64,
}

/// `issueAttachmentUploadUrl`.
pub fn issue_upload_url(userdata: &Path, request: UploadRequest) -> Result<UploadUrl, Failure> {
    let attachment_type = request.attachment_type.unwrap_or_else(|| "image".into());
    let mime_type = request.mime_type;
    let mut name = request.name.unwrap_or_else(|| "attachment".into());
    if attachment_type == "file" && is_svg(&mime_type, &name) && !name.to_lowercase().ends_with(".svg") {
        name.push_str(".svg");
    }
    let ext = (attachment_type == "file").then(|| file_extension(&name));
    let attachment_id = format!("{PENDING}-{}{}", crate::util::new_uuid(), id_suffix(ext.as_deref()));
    let expires_at = domovoi_contracts::time_fmt::now_millis() + UPLOAD_URL_TTL_MS;
    let claims = Claims {
        version: 1,
        kind: "attachment-upload".into(),
        attachment_type,
        attachment_id: attachment_id.clone(),
        name,
        mime_type,
        size_bytes: request.size_bytes,
        expires_at,
    };
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("plain data serializes"));
    let key = signing_key(userdata).map_err(|e| Failure::SigningKey { cause: e.to_string() })?;
    let signature = sign(&key, &payload);
    Ok(UploadUrl { attachment_id, relative_url: format!("{UPLOAD_ROUTE_PREFIX}/{payload}.{signature}"), expires_at })
}

/// `validateAttachmentUploadToken`: a well-formed, correctly signed, unexpired token.
pub fn validate_token(userdata: &Path, token: &str) -> Option<Claims> {
    let mut parts = token.split('.');
    let (payload, signature) = (parts.next()?, parts.next()?);
    if parts.next().is_some() || payload.is_empty() || signature.is_empty() {
        return None;
    }
    let key = signing_key(userdata).ok()?;
    let mut mac = HmacSha256::new_from_slice(&key).ok()?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&URL_SAFE_NO_PAD.decode(signature).ok()?).ok()?;
    let claims: Claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    (claims.version == 1 && claims.kind == "attachment-upload" && claims.expires_at > domovoi_contracts::time_fmt::now_millis()).then_some(claims)
}

/// Where `assets.createUrl` serves an attachment from.
pub const ASSET_ROUTE_PREFIX: &str = "/api/assets/attachment";
/// How long an attachment's URL works (it is signed; nothing else guards it).
const ASSET_URL_TTL_MS: i64 = 7 * 24 * 60 * 60_000;

/// The stored file of an attachment id (`<id><ext>`), if any.
pub fn stored_file(userdata: &Path, attachment_id: &str) -> Option<PathBuf> {
    if attachment_id.is_empty() || attachment_id.contains(['/', '\\', '.']) {
        return None;
    }
    std::fs::read_dir(dir(userdata)).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.is_file() && p.file_stem().is_some_and(|s| s == attachment_id) && p.extension().is_some_and(|e| e != "part")
    })
}

/// `assets.createUrl` for an `attachment` resource: a signed URL that serves
/// the stored file until it expires.
pub fn issue_asset_url(userdata: &Path, attachment_id: &str) -> Result<AssetCreateUrlResult, Failure> {
    let err = |m: &str| Failure::AssetUrl { message: m.to_owned() };
    stored_file(userdata, attachment_id).ok_or_else(|| err("Attachment not found."))?;
    let expires_at = domovoi_contracts::time_fmt::now_millis() + ASSET_URL_TTL_MS;
    let claims = ReadClaims { version: 1, kind: "attachment-read".into(), attachment_id: attachment_id.to_owned(), expires_at };
    let token = sign_asset_claims(userdata, &claims).map_err(|_| err("Could not sign the URL."))?;
    Ok(AssetCreateUrlResult {
        relative_url: format!("{ASSET_ROUTE_PREFIX}/{token}"),
        expires_at: expires_at.into(),
        source_path: None,
        image_dimensions: None,
        file_revision: None,
    })
}

/// The file and content type an attachment URL's token grants, while valid.
pub fn read_asset(userdata: &Path, token: &str) -> Option<(PathBuf, &'static str)> {
    let claims: ReadClaims = verify_asset_claims(userdata, token)?;
    if claims.version != 1 || claims.kind != "attachment-read" || claims.expires_at <= domovoi_contracts::time_fmt::now_millis() {
        return None;
    }
    let file = stored_file(userdata, &claims.attachment_id)?;
    let mime = match file.extension().and_then(|e| e.to_str()).unwrap_or_default() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };
    Some((file, mime))
}

/// `storeAttachmentUpload`: the body must match the claimed size; a second
/// upload of the same id is accepted only with identical bytes.
pub fn store_upload(userdata: &Path, claims: &Claims, body: &[u8]) -> Result<(), (u16, String)> {
    if body.len() as u64 != claims.size_bytes {
        return Err((400, format!("Body was {} bytes, expected {}.", body.len(), claims.size_bytes)));
    }
    let ext = if claims.attachment_type == "file" { file_extension(&claims.name) } else { image_extension(&claims.mime_type, &claims.name) };
    let dir = dir(userdata);
    let final_path = dir.join(format!("{}{ext}", claims.attachment_id));
    let internal = |e: std::io::Error| (500, format!("Failed to persist upload: {e}"));
    std::fs::create_dir_all(&dir).map_err(internal)?;
    let part = dir.join(format!("{}{ext}.{}.part", claims.attachment_id, crate::util::new_uuid()));
    std::fs::write(&part, body).map_err(internal)?;
    let result = match std::fs::hard_link(&part, &final_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(&final_path).map_err(internal)?;
            if Sha256::digest(&existing) == Sha256::digest(body) { Ok(()) } else { Err((409, "Attachment already uploaded.".into())) }
        }
        Err(e) => Err(internal(e)),
    };
    let _ = std::fs::remove_file(&part);
    result
}

/// `deletePendingAttachment`: only uploads not yet adopted by a message.
pub fn delete_pending(userdata: &Path, attachment_id: &str) {
    let Some(rest) = attachment_id.strip_prefix(&format!("{PENDING}-")) else { return };
    if rest.contains(['/', '.', '\\']) {
        return;
    }
    let dir = dir(userdata);
    if let Ok(read) = std::fs::read_dir(&dir) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name.split('.').next().unwrap_or_default();
            if stem == attachment_id && !name.ends_with(".part") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// `parseBase64DataUrl`: `(mime type, base64 payload)`.
fn parse_data_url(url: &str) -> Option<(String, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    Some((mime.trim().to_lowercase(), data.trim()))
}

/// An image sent inline (`UploadChatImageAttachment`). Every field has a
/// fallback, taken too when the field is not of its type: named `attachment`,
/// of no type, with no data and a size that matches none. A `source` that is
/// not where a capture came from is left out.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Upload {
    #[serde(default, deserialize_with = "if_fits")]
    name: Option<String>,
    #[serde(default, deserialize_with = "text_or_empty")]
    mime_type: String,
    #[serde(default, deserialize_with = "if_fits")]
    size_bytes: Option<u64>,
    #[serde(default, deserialize_with = "text_or_empty")]
    data_url: String,
    #[serde(default, deserialize_with = "if_fits")]
    source: Option<ImageSource>,
}

/// `PersistChatAttachmentsInput`. Ids left out read as empty, and anything
/// but a list of attachments as none; an attachment that is no record has
/// every fallback of [`Upload`].
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PersistRequest {
    #[serde(default, deserialize_with = "text_or_empty")]
    thread_id: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    message_id: String,
    #[serde(default, deserialize_with = "uploads")]
    attachments: Vec<Upload>,
}

fn uploads<'de, D: Deserializer<'de>>(sent: D) -> Result<Vec<Upload>, D::Error> {
    let sent: Option<Vec<RecordOrNot<Upload>>> = list_if_list(sent)?;
    Ok(sent.unwrap_or_default().into_iter().map(|RecordOrNot(upload)| upload).collect())
}

/// `persistChatAttachments`: inline image data URLs written under stable ids.
pub fn persist_chat_attachments(userdata: &Path, request: PersistRequest) -> Result<PersistChatAttachmentsResult, Failure> {
    let err = |message: String| Failure::Persist { message };
    let (thread_id, message_id) = (request.thread_id.as_str(), request.message_id.as_str());
    let mut attachments = vec![];
    for (index, a) in request.attachments.into_iter().enumerate() {
        let name = a.name.unwrap_or_else(|| "attachment".into());
        let mime = a.mime_type;
        let (data_mime, b64) = parse_data_url(&a.data_url)
            .filter(|(m, _)| *m == mime.to_lowercase())
            .ok_or_else(|| err(format!("Attachment {name} has an invalid image payload.")))?;
        let bytes = STANDARD.decode(b64).map_err(|_| err(format!("Attachment {name} is not valid base64.")))?;
        if bytes.len() as u64 != a.size_bytes.unwrap_or(u64::MAX) {
            return Err(err(format!("Attachment {name} size does not match its payload.")));
        }
        let stable_key = format!("{message_id}:{index}:{}", hex(&Sha256::digest(&bytes)));
        let id = deterministic_id(thread_id, &stable_key).ok_or_else(|| err("Could not allocate an attachment identifier.".into()))?;
        let dir = dir(userdata);
        std::fs::create_dir_all(&dir).map_err(|_| err(format!("Could not persist attachment {name}.")))?;
        std::fs::write(dir.join(format!("{id}{}", image_extension(&data_mime, &name))), &bytes)
            .map_err(|_| err(format!("Could not persist attachment {name}.")))?;
        attachments.push(ChatAttachment::Image(ImageAttachment { id, name, mime_type: mime, size_bytes: bytes.len() as u64, source: a.source }));
    }
    Ok(PersistChatAttachmentsResult { attachments })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn upload_request(sent: serde_json::Value) -> UploadRequest {
        serde_json::from_value(sent).unwrap()
    }

    fn userdata() -> PathBuf {
        let d = std::env::temp_dir().join(format!("domovoi-att-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn attachment_urls_are_signed_and_serve_only_their_file() {
        let u = userdata();
        std::fs::create_dir_all(dir(&u)).unwrap();
        std::fs::write(dir(&u).join("t1-abc.png"), b"png").unwrap();
        assert!(issue_asset_url(&u, "missing").is_err());
        assert!(issue_asset_url(&u, "../t1-abc").is_err());
        let issued = issue_asset_url(&u, "t1-abc").unwrap();
        let token = issued.relative_url.strip_prefix(&format!("{ASSET_ROUTE_PREFIX}/")).unwrap().to_owned();
        let (file, mime) = read_asset(&u, &token).expect("valid");
        assert_eq!((file.file_name().unwrap().to_str().unwrap(), mime), ("t1-abc.png", "image/png"));
        let (payload, sig) = token.split_once('.').unwrap();
        assert!(read_asset(&u, &format!("{payload}.{}", sig.chars().rev().collect::<String>())).is_none(), "a forged signature");
        // An upload token is not a read token.
        let up = issue_upload_url(&u, upload_request(json!({ "type": "file", "name": "a.md", "mimeType": "text/markdown", "sizeBytes": 1 }))).unwrap();
        let up = up.relative_url.rsplit('/').next().unwrap().to_owned();
        assert!(read_asset(&u, &up).is_none());
    }

    #[test]
    fn upload_tokens_are_signed_and_single_purpose() {
        let u = userdata();
        let issued = issue_upload_url(&u, upload_request(json!({ "type": "file", "name": "notes.MD", "mimeType": "text/markdown", "sizeBytes": 5 }))).unwrap();
        let id = issued.attachment_id.as_str();
        assert!(id.starts_with("pending-") && id.ends_with("-md"), "{id}");
        let token = issued.relative_url.strip_prefix(&format!("{UPLOAD_ROUTE_PREFIX}/")).unwrap().to_owned();
        let claims = validate_token(&u, &token).expect("valid");
        assert_eq!(claims.attachment_id, id);
        let (payload, sig) = token.split_once('.').unwrap();
        let mut forged = sig.to_owned();
        forged.replace_range(..1, if sig.starts_with('A') { "B" } else { "A" });
        assert!(validate_token(&u, &format!("{payload}.{forged}")).is_none());
        assert!(validate_token(&u, payload).is_none());

        assert_eq!(store_upload(&u, &claims, b"1234").unwrap_err().0, 400);
        store_upload(&u, &claims, b"hello").unwrap();
        store_upload(&u, &claims, b"hello").unwrap();
        assert_eq!(store_upload(&u, &claims, b"world").unwrap_err().0, 409);
        assert_eq!(std::fs::read(dir(&u).join(format!("{id}.md"))).unwrap(), b"hello");
        delete_pending(&u, id);
        assert!(!dir(&u).join(format!("{id}.md")).exists());
        std::fs::remove_dir_all(u).unwrap();
    }

    #[test]
    fn chat_attachments_get_stable_ids() {
        let u = userdata();
        let input = json!({ "threadId": "Thread 1!", "messageId": "m1", "attachments": [
            { "type": "image", "name": "a.png", "mimeType": "image/png", "sizeBytes": 3, "dataUrl": "data:image/png;base64,AQID" }
        ] });
        let persist = |sent: &serde_json::Value| persist_chat_attachments(&u, serde_json::from_value(sent.clone()).unwrap()).map(|kept| json!(kept));
        let a = persist(&input).unwrap();
        let b = persist(&input).unwrap();
        assert_eq!(a, b, "retries keep ids");
        let id = a["attachments"][0]["id"].as_str().unwrap();
        assert!(id.starts_with("thread-1-"), "{id}");
        assert_eq!(std::fs::read(dir(&u).join(format!("{id}.png"))).unwrap(), [1, 2, 3]);
        let mut bad = input.clone();
        bad["attachments"][0]["sizeBytes"] = json!(4);
        assert_eq!(
            json!(persist(&bad).unwrap_err()),
            json!({ "_tag": "PersistChatAttachmentsError", "message": "Attachment a.png size does not match its payload." })
        );
        std::fs::remove_dir_all(u).unwrap();
    }

    #[test]
    fn segments_and_extensions_follow_t3code() {
        assert_eq!(thread_segment("pending").as_deref(), Some("_pending"));
        assert_eq!(thread_segment("  ").as_deref(), None);
        assert_eq!(file_extension("x.part"), ".bin");
        assert_eq!(file_extension("archive.tar.GZ"), ".gz");
        assert_eq!(file_extension("noext"), ".bin");
    }

    #[test]
    fn failures_are_written_as_tagged_errors() {
        assert_eq!(
            json!(Failure::SigningKey { cause: "Is a directory (os error 21)".into() }),
            json!({ "_tag": "AttachmentUploadSigningKeyError", "cause": "Is a directory (os error 21)" })
        );
        assert_eq!(json!(Failure::AssetUrl { message: "Attachment not found.".into() }), json!({ "_tag": "AssetUrlError", "message": "Attachment not found." }));
        assert_eq!(
            json!(Failure::Persist { message: "Could not allocate an attachment identifier.".into() }),
            json!({ "_tag": "PersistChatAttachmentsError", "message": "Could not allocate an attachment identifier." })
        );
    }

    #[test]
    fn what_is_asked_with_has_a_fallback_for_every_field() {
        let asked: UploadRequest = serde_json::from_value(json!({ "type": 5, "name": null, "mimeType": [], "sizeBytes": "big" })).unwrap();
        assert_eq!((asked.attachment_type, asked.name, asked.mime_type.as_str(), asked.size_bytes), (None, None, "", 0));
        let asked: PersistRequest = serde_json::from_value(json!({ "threadId": 1, "attachments": [5, { "name": "a.png", "sizeBytes": 3, "source": { "kind": "other" } }] })).unwrap();
        assert_eq!((asked.thread_id.as_str(), asked.message_id.as_str(), asked.attachments.len()), ("", "", 2));
        assert_eq!((asked.attachments[0].name.as_deref(), asked.attachments[0].size_bytes), (None, None));
        assert_eq!((asked.attachments[1].name.as_deref(), asked.attachments[1].size_bytes, &asked.attachments[1].source), (Some("a.png"), Some(3), &None));
        let asked: PersistRequest = serde_json::from_value(json!({ "attachments": "none" })).unwrap();
        assert!(asked.attachments.is_empty());
    }
}
