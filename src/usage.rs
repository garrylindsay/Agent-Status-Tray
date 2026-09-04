//! What Cursor charged for a conversation, asked of Cursor.
//!
//! Cursor writes no usable cost to disk. `usageData.costInCents` on a composer is zero on all but a
//! handful of old records, its per-message token counts are almost always zero and carry no model,
//! and cloud agents record a model with no tokens at all. The number only exists on Cursor's own
//! dashboard, so that is where it is fetched from.
//!
//! The join is `conversationId` on each usage event: a bare uuid for a local chat, matching
//! `composerData:<id>`, and a `bc-` prefixed one for a cloud agent, matching its `bcId`.
//!
//! Nothing here is a credential the tray owns. Cursor's own session token already sits in the store
//! this program reads, and it is read at the moment of the call, used, and dropped -- never copied
//! into the tray's config, never written anywhere. Failure is always survivable: the map comes back
//! empty, Cursor rows show no cost exactly as they did before, and the reason goes to the log so
//! that "no cost" is never mistaken for "cost nothing".

use std::collections::HashMap;

use serde::Deserialize;

use crate::log;

const HOST: &str = "cursor.com";
const PATH: &str = "/api/dashboard/get-filtered-usage-events";

/// The dashboard's own page size. Ten pages is far more history than the tray lists.
const PAGE_SIZE: u32 = 500;
const MAX_PAGES: u32 = 10;

/// A network call is not a poll. The dashboard bills per request and the numbers barely move, so
/// this runs on its own slow clock rather than with the file scans.
const REFRESH_MS: u64 = 10 * 60 * 1000;

/// How long to wait before trying again after a failure, so a dead endpoint or an expired token is
/// not hammered every ten minutes for a number that is not coming.
const RETRY_MS: u64 = 30 * 60 * 1000;

#[derive(Deserialize)]
struct Page {
    #[serde(rename = "usageEventsDisplay")]
    events: Option<Vec<Event>>,
    #[serde(rename = "totalUsageEventsCount")]
    total: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "conversationId")]
    conversation: Option<String>,
    #[serde(rename = "tokenUsage")]
    tokens: Option<TokenUsage>,
}

#[derive(Deserialize)]
struct TokenUsage {
    #[serde(rename = "totalCents")]
    cents: Option<f64>,
}

/// Cursor's session cookie and the team the dashboard is scoped to.
struct Credentials {
    cookie: String,
    team: i64,
}

/// Percent-encodes everything that is not unreserved, which is what the cookie value needs: the
/// account id in it carries a `|`.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Decodes one base64url segment. Only ever used on a JWT's payload, to read the account id the
/// cookie has to be built from -- the signature is Cursor's business and is never touched.
fn base64url(segment: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for byte in segment.bytes() {
        if byte == b'=' {
            break;
        }
        let value = TABLE.iter().position(|c| *c == byte)? as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn read_credentials() -> Result<Credentials, String> {
    let base = std::env::var("APPDATA").map_err(|_| "no APPDATA".to_string())?;
    let path = std::path::PathBuf::from(base)
        .join("Cursor")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb");
    if !path.exists() {
        return Err("Cursor is not installed".to_string());
    }

    let uri = format!("file:{}?mode=ro", path.to_string_lossy().replace('\\', "/"));
    let connection = rusqlite::Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| format!("could not open Cursor's store ({e})"))?;

    let item = |key: &str| -> Option<String> {
        connection
            .query_row("select value from ItemTable where key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .ok()
    };

    let token = item("cursorAuth/accessToken").ok_or("not signed in to Cursor")?;

    // The cookie is the account id and the token together; the id is in the token's own payload.
    let payload = token.split('.').nth(1).ok_or("unexpected token format")?;
    let claims: serde_json::Value = base64url(payload)
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or("unexpected token format")?;
    let subject = claims
        .get("sub")
        .and_then(|s| s.as_str())
        .ok_or("unexpected token format")?;

    let team = item("cursorAuth/cachedTeam")
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("teamId").and_then(|t| t.as_i64()))
        .ok_or("no Cursor team on this machine")?;

    Ok(Credentials {
        cookie: format!(
            "WorkosCursorSessionToken={}%3A%3A{}",
            encode(subject),
            token
        ),
        team,
    })
}

/// Totals every usage event by the conversation it belongs to.
fn fetch(credentials: &Credentials) -> Result<HashMap<String, f64>, String> {
    // The endpoint refuses a bare token: it wants the request to look like it came from the
    // dashboard, which means the origin and referrer as well as the cookie.
    let headers = format!(
        "Content-Type: application/json\r\n\
         User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36\r\n\
         Origin: https://{HOST}\r\n\
         Referer: https://{HOST}/dashboard/usage\r\n\
         Cookie: {}",
        credentials.cookie
    );

    let mut totals: HashMap<String, f64> = HashMap::new();
    let mut seen = 0usize;

    for page in 1..=MAX_PAGES {
        let body = format!(
            r#"{{"teamId":{},"page":{page},"pageSize":{PAGE_SIZE}}}"#,
            credentials.team
        );
        let response = crate::http::post_json(HOST, PATH, &headers, body.as_bytes())?;

        match response.status {
            200 => {}
            401 | 403 => return Err("Cursor rejected the session (sign in to Cursor again)".into()),
            other => return Err(format!("Cursor returned HTTP {other}")),
        }

        let parsed: Page = serde_json::from_str(&response.body)
            .map_err(|_| "Cursor's reply was not what was expected".to_string())?;
        let events = parsed.events.unwrap_or_default();
        if events.is_empty() {
            break;
        }
        seen += events.len();

        for event in events {
            let (Some(id), Some(cents)) = (
                event.conversation.filter(|id| !id.is_empty() && id != "null"),
                event.tokens.and_then(|t| t.cents),
            ) else {
                continue;
            };
            *totals.entry(id).or_insert(0.0) += cents / 100.0;
        }

        // The count is quoted as a string on some replies and a number on others.
        let total = parsed
            .total
            .as_ref()
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0) as usize;
        if total > 0 && seen >= total {
            break;
        }
    }

    Ok(totals)
}

#[derive(Default)]
pub struct Usage {
    by_conversation: HashMap<String, f64>,
    /// When the next call is allowed, so a failure backs off instead of retrying every refresh.
    next_ms: u64,
    /// Whether a call has ever succeeded, so the first failure can say so plainly.
    ever: bool,
}

impl Usage {
    pub fn new() -> Usage {
        Usage::default()
    }

    /// Refreshes on its own slow clock. Cheap to call every poll.
    pub fn refresh(&mut self, now_ms: u64, enabled: bool) {
        if !enabled {
            if !self.by_conversation.is_empty() {
                self.by_conversation.clear();
            }
            return;
        }
        if now_ms < self.next_ms {
            return;
        }

        match read_credentials().and_then(|c| fetch(&c)) {
            Ok(totals) => {
                if totals.is_empty() && !self.ever {
                    log::record("Cursor reported no usage events", now_ms);
                }
                self.by_conversation = totals;
                self.ever = true;
                self.next_ms = now_ms + REFRESH_MS;
            }
            Err(why) => {
                log::record(format!("Cursor usage: {why}"), now_ms);
                // The last good numbers are kept rather than blanked: a conversation's cost does
                // not stop being true because the network went away.
                self.next_ms = now_ms + RETRY_MS;
            }
        }
    }

    /// What Cursor charged for a conversation, if it said.
    pub fn get(&self, conversation_id: &str) -> Option<f64> {
        self.by_conversation.get(conversation_id).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_id_is_escaped_for_a_cookie() {
        assert_eq!(encode("github|user_01ABC"), "github%7Cuser_01ABC");
        assert_eq!(encode("plain"), "plain");
        assert_eq!(encode("a.b-c_d~e"), "a.b-c_d~e");
    }

    #[test]
    fn a_jwt_payload_decodes() {
        // {"sub":"github|u1"} with the padding a JWT omits.
        let segment = "eyJzdWIiOiJnaXRodWJ8dTEifQ";
        let bytes = base64url(segment).expect("decodes");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(value.get("sub").and_then(|s| s.as_str()), Some("github|u1"));
    }

    #[test]
    fn rubbish_is_not_mistaken_for_a_payload() {
        assert!(base64url("not valid!!").is_none());
    }

    /// Events are totalled per conversation, and anything without both an id and a charge is
    /// skipped rather than counted as zero.
    #[test]
    fn events_total_by_conversation() {
        let json = r#"{"totalUsageEventsCount":4,"usageEventsDisplay":[
            {"conversationId":"a","tokenUsage":{"totalCents":100.0}},
            {"conversationId":"a","tokenUsage":{"totalCents":50.0}},
            {"conversationId":"bc-b","tokenUsage":{"totalCents":25.0}},
            {"conversationId":"null","tokenUsage":{"totalCents":999.0}},
            {"conversationId":"c"}
        ]}"#;
        let page: Page = serde_json::from_str(json).unwrap();
        let mut totals: HashMap<String, f64> = HashMap::new();
        for event in page.events.unwrap() {
            let (Some(id), Some(cents)) = (
                event.conversation.filter(|id| !id.is_empty() && id != "null"),
                event.tokens.and_then(|t| t.cents),
            ) else {
                continue;
            };
            *totals.entry(id).or_insert(0.0) += cents / 100.0;
        }
        assert_eq!(totals.get("a"), Some(&1.5));
        assert_eq!(totals.get("bc-b"), Some(&0.25));
        assert_eq!(totals.get("null"), None, "an unattributed event is not a conversation");
        assert_eq!(totals.get("c"), None, "an event with no charge is not a zero");
    }

    #[test]
    fn switching_it_off_forgets_what_was_fetched() {
        let mut usage = Usage::new();
        usage.by_conversation.insert("a".to_string(), 1.0);
        usage.refresh(0, false);
        assert_eq!(usage.get("a"), None);
    }

    /// Asks Cursor for real, using whatever this machine is signed in as.
    /// `cargo test -- --nocapture live_usage`
    #[test]
    fn live_usage() {
        match read_credentials() {
            Err(why) => println!("no credentials: {why}"),
            Ok(credentials) => match fetch(&credentials) {
                Err(why) => println!("fetch failed: {why}"),
                Ok(totals) => {
                    let mut rows: Vec<_> = totals.iter().collect();
                    rows.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());
                    println!("{} conversations charged", rows.len());
                    for (id, usd) in rows.iter().take(8) {
                        println!("  ${:<8.2} {id}", usd);
                    }
                }
            },
        }
    }
}
