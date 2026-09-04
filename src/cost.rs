//! What a session has cost, totalled from Claude Code's own transcripts.
//!
//! Every assistant turn records a `usage` block, and the models have published per-token prices, so
//! the spend of a conversation can be added up from the file it already writes. Nothing here talks
//! to an API; it is arithmetic over bytes already on disk.
//!
//! A conversation is also not one continuous run. Claude's own usage report totals only the run
//! you are in, so a session picked up again after a night reads far cheaper there than its whole
//! transcript has cost. Both totals are kept as the file is walked, and the setting picks which to
//! show -- keeping both means changing the setting never costs a re-read.
//!
//! Two properties of the transcripts shape this module:
//!
//! * **They only ever grow.** So a session is parsed once and then only from where the last read
//!   stopped -- there are 100MB of transcripts on a normal machine and this runs every poll.
//! * **One API response becomes several lines.** A turn with text and a tool call is written as one
//!   line per content block, each carrying an identical copy of the same `usage`. Counting lines
//!   rather than responses roughly doubles the total.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use serde::Deserialize;

use crate::config::CostScope;
use crate::title::transcript_path;

/// Dollars per million tokens, as published for the Claude API.
struct Price {
    input: f64,
    output: f64,
    /// Cache reads are a tenth of the input price on every model but Fable, which is a fortieth.
    read: f64,
}

/// Cache writes are charged above the input price: a quarter more for the 5-minute TTL, double for
/// the 1-hour one. Claude Code uses both, and the gap between them is too wide to average over.
const WRITE_5M: f64 = 1.25;
const WRITE_1H: f64 = 2.0;

/// Price for a model id, or `None` for one this build has never heard of.
///
/// An unknown model is deliberately left out of the total rather than guessed at: a wrong number
/// shown confidently is worse than no number, and a new model would otherwise be priced as whatever
/// the fallback happened to be. The cost of being wrong here is silent, which is why it is refused.
fn price(model: &str) -> Option<Price> {
    let (input, output) = match model {
        "claude-fable-5" | "claude-fable-5-1" | "claude-mythos-5-1" => (10.0, 50.0),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => (5.0, 25.0),
        "claude-sonnet-5" => (2.0, 10.0),
        "claude-sonnet-4-6" => (3.0, 15.0),
        "claude-haiku-4-5" => (1.0, 5.0),
        _ => return None,
    };
    // Fable's cheaper cache reads are a property of the model, not a discount to apply elsewhere.
    let read = if model.contains("fable") || model.contains("mythos") {
        input * 0.025
    } else {
        input * 0.1
    };
    Some(Price { input, output, read })
}

#[derive(Deserialize)]
struct Line {
    message: Option<Message>,
    /// When the turn happened, which is what separates one run from the next.
    timestamp: Option<String>,
}

/// Epoch ms for an ISO-8601 UTC stamp like `2026-09-03T18:32:21.758Z`.
///
/// Only differences between two of these are ever taken, so leap seconds and sub-second precision
/// do not matter; the calendar arithmetic does, because a run boundary can fall across midnight,
/// a month end or a year end.
fn epoch_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' {
        return None;
    }
    let num = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);

    // Days from 1970-01-01, by Howard Hinnant's civil-date algorithm.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    Some(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000)
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    /// Splits the write above by TTL. Absent on older records, which is what the total is for.
    cache_creation: Option<CacheCreation>,
}

#[derive(Deserialize, Default)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

impl Usage {
    fn dollars(&self, price: &Price) -> f64 {
        let split = self.cache_creation.as_ref();
        let (write_5m, write_1h) = match split {
            // Both zero means the record predates the split; fall back to the undifferentiated
            // total at the cheaper rate rather than inventing a TTL that was never recorded.
            Some(c) if c.ephemeral_5m_input_tokens > 0 || c.ephemeral_1h_input_tokens > 0 => {
                (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens)
            }
            _ => (self.cache_creation_input_tokens, 0),
        };

        let tokens = self.input_tokens as f64 * price.input
            + self.output_tokens as f64 * price.output
            + self.cache_read_input_tokens as f64 * price.read
            + write_5m as f64 * price.input * WRITE_5M
            + write_1h as f64 * price.input * WRITE_1H;
        tokens / 1_000_000.0
    }
}

/// How far through a transcript the running totals have been carried.
struct Cached {
    /// Bytes consumed, always ending on a line boundary so the next read resumes cleanly.
    offset: u64,
    /// The last response counted. Repeats of one response are always adjacent, so remembering one
    /// id is enough to skip them -- no set of every id in the conversation has to be kept.
    last_id: Option<String>,
    /// When the last counted response happened, so the next one can be told whether it belongs to
    /// the same run.
    last_ms: Option<i64>,
    /// Everything the conversation has ever cost.
    lifetime: f64,
    /// Only what has been spent since the last long gap.
    run: f64,
}

#[derive(Default)]
pub struct Costs {
    cache: HashMap<String, Cached>,
}

impl Costs {
    pub fn new() -> Costs {
        Costs::default()
    }

    /// Dollars spent by a session, or `None` when it has no transcript to read.
    ///
    /// `gap_mins` is how long a silence has to be to count as the end of a run. Cursor sessions
    /// never reach here: Cursor records no token usage on disk, so there is nothing to add up.
    pub fn get(
        &mut self,
        cwd: &str,
        session_id: &str,
        scope: CostScope,
        gap_mins: u64,
    ) -> Option<f64> {
        if scope == CostScope::Off {
            return None;
        }
        let path = transcript_path(cwd, session_id)?;
        let len = std::fs::metadata(&path).ok()?.len();

        let entry = self.cache.entry(session_id.to_string()).or_insert(Cached {
            offset: 0,
            last_id: None,
            last_ms: None,
            lifetime: 0.0,
            run: 0.0,
        });

        // A transcript that shrank is a different conversation in the same place; start over rather
        // than resume from an offset that now points into the middle of unrelated bytes.
        if len < entry.offset {
            *entry = Cached {
                offset: 0,
                last_id: None,
                last_ms: None,
                lifetime: 0.0,
                run: 0.0,
            };
        }

        if len > entry.offset
            && let Some(text) = read_from(&path, entry.offset)
            // The tail after the last newline is a line still being written. Leaving it unconsumed
            // lets the next poll read it whole, so the offset advances by complete lines only.
            && let Some(last) = text.rfind('\n')
        {
            let complete = &text[..=last];
            let gap_ms = (gap_mins.max(1) * 60_000) as i64;

            for line in complete.lines() {
                let Ok(parsed) = serde_json::from_str::<Line>(line) else {
                    continue;
                };
                let Some(message) = parsed.message else { continue };
                let (Some(usage), Some(model)) = (message.usage, message.model) else {
                    continue;
                };
                if message.id.is_some() && message.id == entry.last_id {
                    continue;
                }
                entry.last_id = message.id;

                let Some(price) = price(&model) else { continue };
                let usd = usage.dollars(&price);
                entry.lifetime += usd;

                // A silence longer than the gap means the context went cold and was rebuilt, which
                // is exactly where Claude's own report starts counting again.
                let at = parsed.timestamp.as_deref().and_then(epoch_ms);
                let fresh = matches!((at, entry.last_ms), (Some(now), Some(prev)) if now - prev > gap_ms);
                entry.run = if fresh { usd } else { entry.run + usd };
                if at.is_some() {
                    entry.last_ms = at;
                }
            }
            entry.offset += complete.len() as u64;
        }

        Some(match scope {
            CostScope::Conversation => entry.lifetime,
            _ => entry.run,
        })
    }

    /// Drops sessions that are no longer running, so the cache cannot grow without bound.
    pub fn retain(&mut self, live: &[String]) {
        self.cache.retain(|id, _| live.iter().any(|l| l == id));
    }
}

fn read_from(path: &PathBuf, offset: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// `$4.12`, or `$0.08` for small amounts. Whole dollars past a hundred, where cents are noise.
pub fn format(usd: f64) -> String {
    if usd >= 100.0 {
        format!("${:.0}", usd)
    } else {
        format!("${usd:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, model: &str, usage: &str) -> String {
        format!(r#"{{"type":"assistant","message":{{"id":"{id}","model":"{model}","usage":{usage}}}}}"#)
    }

    #[test]
    fn output_and_input_are_priced_per_million() {
        let usage: Usage =
            serde_json::from_str(r#"{"input_tokens":1000000,"output_tokens":1000000}"#).unwrap();
        // Opus 5: $5 in, $25 out.
        assert_eq!(usage.dollars(&price("claude-opus-5").unwrap()), 30.0);
    }

    #[test]
    fn cache_reads_are_a_tenth_of_input() {
        let usage: Usage = serde_json::from_str(r#"{"cache_read_input_tokens":1000000}"#).unwrap();
        assert_eq!(usage.dollars(&price("claude-opus-5").unwrap()), 0.5);
    }

    /// The 1-hour TTL costs double the input price and the 5-minute one a quarter more, so a
    /// transcript that used the long TTL cannot be priced as if it used the short one.
    #[test]
    fn the_two_cache_ttls_are_priced_apart() {
        let short: Usage = serde_json::from_str(
            r#"{"cache_creation_input_tokens":1000000,
                "cache_creation":{"ephemeral_5m_input_tokens":1000000,"ephemeral_1h_input_tokens":0}}"#,
        )
        .unwrap();
        let long: Usage = serde_json::from_str(
            r#"{"cache_creation_input_tokens":1000000,
                "cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":1000000}}"#,
        )
        .unwrap();
        let opus = price("claude-opus-5").unwrap();
        assert_eq!(short.dollars(&opus), 6.25);
        assert_eq!(long.dollars(&opus), 10.0);
    }

    /// Records written before the TTL split carry only the total.
    #[test]
    fn a_write_with_no_ttl_split_still_counts() {
        let usage: Usage =
            serde_json::from_str(r#"{"cache_creation_input_tokens":1000000}"#).unwrap();
        assert_eq!(usage.dollars(&price("claude-opus-5").unwrap()), 6.25);
    }

    #[test]
    fn unknown_models_have_no_price() {
        assert!(price("claude-opus-9").is_none());
        assert!(price("<synthetic>").is_none());
        assert!(price("").is_none());
    }

    #[test]
    fn fable_reads_cheaper_than_opus_despite_costing_more_to_run() {
        let fable = price("claude-fable-5-1").unwrap();
        let opus = price("claude-opus-5").unwrap();
        assert!(fable.input > opus.input);
        assert!(fable.read < opus.read);
    }

    #[test]
    fn money_reads_as_money() {
        assert_eq!(format(0.0), "$0.00");
        assert_eq!(format(0.083), "$0.08");
        assert_eq!(format(4.128), "$4.13");
        assert_eq!(format(187.3), "$187");
    }

    /// The same response split across a text line and a tool_use line must be counted once.
    #[test]
    fn one_response_written_as_two_lines_is_counted_once() {
        let usage = r#"{"input_tokens":1000000,"output_tokens":0}"#;
        let text = format!(
            "{}\n{}\n",
            line("msg_1", "claude-opus-5", usage),
            line("msg_1", "claude-opus-5", usage)
        );
        assert_eq!(total_of(&text), 5.0);
    }

    #[test]
    fn distinct_responses_both_count() {
        let usage = r#"{"input_tokens":1000000,"output_tokens":0}"#;
        let text = format!(
            "{}\n{}\n",
            line("msg_1", "claude-opus-5", usage),
            line("msg_2", "claude-opus-5", usage)
        );
        assert_eq!(total_of(&text), 10.0);
    }

    /// Resuming from an offset must produce the same total as reading it all at once, including
    /// when the split lands inside a line.
    #[test]
    fn reading_in_pieces_matches_reading_it_whole() {
        let usage = r#"{"input_tokens":1000000,"output_tokens":0}"#;
        let text = format!(
            "{}\n{}\n{}\n",
            line("msg_1", "claude-opus-5", usage),
            line("msg_2", "claude-opus-5", usage),
            line("msg_3", "claude-opus-5", usage)
        );

        let whole = total_of(&text);
        assert_eq!(whole, 15.0);

        for cut in 1..text.len() {
            let (head, tail) = text.split_at(cut);
            let mut state = (0.0, None::<String>);
            let consumed = feed(head, &mut state);
            feed(&text[consumed..], &mut state);
            assert_eq!(state.0, whole, "split at {cut} disagreed with a whole read");
            let _ = tail;
        }
    }

    /// The line-walking half of `get`, over text rather than a file, so the accounting can be
    /// tested without one. Returns the bytes consumed.
    fn feed(text: &str, state: &mut (f64, Option<String>)) -> usize {
        let Some(last) = text.rfind('\n') else {
            return 0;
        };
        let complete = &text[..=last];
        for line in complete.lines() {
            let Ok(parsed) = serde_json::from_str::<Line>(line) else {
                continue;
            };
            let Some(message) = parsed.message else { continue };
            let (Some(usage), Some(model)) = (message.usage, message.model) else {
                continue;
            };
            if message.id.is_some() && message.id == state.1 {
                continue;
            }
            state.1 = message.id;
            if let Some(price) = price(&model) {
                state.0 += usage.dollars(&price);
            }
        }
        complete.len()
    }

    fn total_of(text: &str) -> f64 {
        let mut state = (0.0, None::<String>);
        feed(text, &mut state);
        state.0
    }

    /// Timestamps have to survive the boundaries a naive parser gets wrong.
    #[test]
    fn timestamps_become_comparable_instants() {
        assert_eq!(epoch_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(epoch_ms("2026-09-03T18:32:21.758Z"), Some(1_788_460_341_000));
        // A minute apart across midnight, a month end and a year end.
        for (a, b) in [
            ("2026-09-03T23:59:30Z", "2026-09-04T00:00:30Z"),
            ("2026-08-31T23:59:30Z", "2026-09-01T00:00:30Z"),
            ("2026-12-31T23:59:30Z", "2027-01-01T00:00:30Z"),
            // Across a leap day.
            ("2028-02-28T23:59:30Z", "2028-02-29T00:00:30Z"),
        ] {
            assert_eq!(epoch_ms(b).unwrap() - epoch_ms(a).unwrap(), 60_000, "{a} -> {b}");
        }
        assert_eq!(epoch_ms("not a timestamp"), None);
        assert_eq!(epoch_ms(""), None);
    }

    /// A file this program is reading must stay replaceable by the program that owns it.
    ///
    /// This is the whole "display only" claim in one test. Claude Code and Cursor rewrite these
    /// files constantly, usually by writing a temporary file and renaming it over the old one. On
    /// Windows an open handle can block exactly that, and the failure would not look like a tray
    /// bug -- it would look like the other application refusing to start because a file is in use.
    /// Every read here goes through `read_from`, so proving its handle shares rename and delete
    /// proves the tray cannot be the thing standing in the way.
    #[test]
    fn a_file_being_read_can_still_be_replaced_and_deleted() {
        let dir = std::env::temp_dir().join(format!("tray-share-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("transcript.jsonl");
        std::fs::write(&path, b"{}
").unwrap();

        let held = File::open(&path).expect("open for reading");

        // What an atomic rewrite does: write a new file, then rename it over the open one.
        let replacement = dir.join("transcript.jsonl.tmp");
        std::fs::write(&replacement, b"{\"new\":true}
").unwrap();
        std::fs::rename(&replacement, &path)
            .expect("a file the tray is reading must still be replaceable");

        std::fs::remove_file(&path).expect("a file the tray is reading must still be deletable");

        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Totals this machine's own sessions both ways.
    /// `cargo test -- --nocapture live_costs`
    #[test]
    fn live_costs() {
        let mut costs = Costs::new();
        for session in crate::session::Registry::new().scan() {
            let Some(id) = session.session_id.as_deref() else { continue };
            let run = costs.get(&session.cwd, id, CostScope::Run, 60);
            let all = costs.get(&session.cwd, id, CostScope::Conversation, 60);
            println!(
                "{:<24} run {:>8}   conversation {:>8}",
                session.name,
                run.map(format).unwrap_or_default(),
                all.map(format).unwrap_or_default()
            );
        }
    }
}

