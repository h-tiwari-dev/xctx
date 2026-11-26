//! The wire format: one JSON object per line, and protocol v2's `rpc_chunk` frames for objects that do not fit
//! in one line (base64 segments of one UTF-8 JSON object, validated as `omp://rpc.md` demands).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

use crate::error::{OmpError, Result};

/// The most bytes a reassembled logical frame may have when the `ready` frame does not say (64 MiB).
pub const DEFAULT_MAX_REASSEMBLED: usize = 64 * 1024 * 1024;

/// A chunked object being collected.
#[derive(Debug)]
struct Partial {
    chunk_id: String,
    count: u64,
    byte_length: usize,
    next: u64,
    bytes: Vec<u8>,
}

/// Turns the lines omp writes into logical frames: a plain line is one frame, and a run of `rpc_chunk` lines is
/// one frame once the last chunk arrives.
#[derive(Debug)]
pub struct Decoder {
    max_reassembled: usize,
    partial: Option<Partial>,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new(DEFAULT_MAX_REASSEMBLED)
    }
}

impl Decoder {
    /// A decoder that refuses logical frames over `max_reassembled` bytes (the `ready` frame's
    /// `maxReassembledFrameBytes`).
    pub fn new(max_reassembled: usize) -> Self {
        Decoder {
            max_reassembled,
            partial: None,
        }
    }

    /// Feed one line (without its newline). Returns the logical frame it completes, or `None` for a chunk that
    /// is not the last, a blank line, or the start of a chunk run.
    ///
    /// A chunk that breaks a rule (wrong id, index or count, an interleaved frame, a bad length, bad base64 or
    /// UTF-8, an object over the limit) is an error and drops the run, so the next frame decodes normally.
    pub fn push_line(&mut self, line: &str) -> Result<Option<Value>> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        let value: Value = serde_json::from_str(line)?;
        if value.get("type").and_then(Value::as_str) != Some("rpc_chunk") {
            if self.partial.take().is_some() {
                return Err(OmpError::Chunk(
                    "a frame arrived between the chunks of another".into(),
                ));
            }
            return Ok(Some(value));
        }
        self.push_chunk(&value)
    }

    fn push_chunk(&mut self, chunk: &Value) -> Result<Option<Value>> {
        let result = self.accept_chunk(chunk);
        if result.is_err() {
            self.partial = None;
        }
        result
    }

    fn accept_chunk(&mut self, chunk: &Value) -> Result<Option<Value>> {
        let bad = |what: &str| OmpError::Chunk(what.to_owned());
        let chunk_id = chunk
            .get("chunkId")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("a chunk has no chunkId"))?;
        let index = chunk
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("a chunk has no index"))?;
        let count = chunk
            .get("count")
            .and_then(Value::as_u64)
            .filter(|c| *c > 0)
            .ok_or_else(|| bad("a chunk has no count"))?;
        let byte_length = chunk
            .get("byteLength")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| bad("a chunk has no byteLength"))?;
        let data = chunk
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("a chunk has no data"))?;
        if byte_length > self.max_reassembled {
            return Err(bad("the object is over the reassembly limit"));
        }
        match &mut self.partial {
            None => {
                if index != 0 {
                    return Err(bad("a chunk run does not start at index 0"));
                }
                self.partial = Some(Partial {
                    chunk_id: chunk_id.to_owned(),
                    count,
                    byte_length,
                    next: 0,
                    bytes: Vec::with_capacity(byte_length),
                });
            }
            Some(p) => {
                if p.chunk_id != chunk_id || p.count != count || p.byte_length != byte_length {
                    return Err(bad("a chunk disagrees with the run it belongs to"));
                }
            }
        }
        let Some(partial) = self.partial.as_mut() else {
            return Err(bad("no chunk run"));
        };
        if index != partial.next {
            return Err(bad("chunks are out of order"));
        }
        let decoded = STANDARD
            .decode(data)
            .map_err(|e| bad(&format!("a chunk is not base64: {e}")))?;
        if partial.bytes.len() + decoded.len() > partial.byte_length {
            return Err(bad("the chunks are longer than byteLength"));
        }
        partial.bytes.extend_from_slice(&decoded);
        partial.next += 1;
        if partial.next < partial.count {
            return Ok(None);
        }
        let Some(done) = self.partial.take() else {
            return Err(bad("no chunk run"));
        };
        if done.bytes.len() != done.byte_length {
            return Err(bad("the chunks are shorter than byteLength"));
        }
        let text = String::from_utf8(done.bytes)
            .map_err(|_| bad("the reassembled object is not UTF-8"))?;
        Ok(Some(serde_json::from_str(&text)?))
    }
}

/// One command or UI response as the line omp reads: compact JSON and a newline.
pub fn encode_line(value: &Value) -> String {
    let mut line = value.to_string();
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn chunks(value: &Value, size: usize, id: &str) -> Vec<String> {
        let text = value.to_string();
        let bytes = text.as_bytes();
        let parts: Vec<&[u8]> = bytes.chunks(size).collect();
        parts
            .iter()
            .enumerate()
            .map(|(i, part)| {
                json!({
                    "type": "rpc_chunk", "chunkId": id, "index": i, "count": parts.len(),
                    "byteLength": bytes.len(), "data": STANDARD.encode(part),
                })
                .to_string()
            })
            .collect()
    }

    #[test]
    fn a_plain_line_is_one_frame_and_a_blank_line_is_none() {
        let mut d = Decoder::default();
        assert_eq!(d.push_line("  ").ok(), Some(None));
        let frame = d.push_line(r#"{"type":"agent_start"}"#).ok().flatten();
        assert_eq!(frame, Some(json!({"type": "agent_start"})));
    }

    #[test]
    fn chunks_reassemble_into_the_original_object_even_across_a_multibyte_split() {
        let original = json!({"type": "response", "data": "héllo wörld ✓ ".repeat(40)});
        // A size that splits inside a multi-byte character: the bytes are joined before UTF-8 is checked.
        let lines = chunks(&original, 7, "rpc-1");
        assert!(lines.len() > 3);
        let mut d = Decoder::default();
        let mut out = None;
        for (i, line) in lines.iter().enumerate() {
            let r = d.push_line(line).expect("valid chunk");
            if i + 1 < lines.len() {
                assert!(r.is_none());
            } else {
                out = r;
            }
        }
        assert_eq!(out, Some(original));
    }

    #[test]
    fn an_interleaved_frame_breaks_the_run_and_the_next_frame_decodes() {
        let lines = chunks(&json!({"type": "x", "pad": "y".repeat(100)}), 40, "rpc-2");
        let mut d = Decoder::default();
        assert!(d.push_line(&lines[0]).expect("first chunk").is_none());
        assert!(matches!(
            d.push_line(r#"{"type":"agent_start"}"#),
            Err(OmpError::Chunk(_))
        ));
        assert!(d.push_line(r#"{"type":"agent_start"}"#).is_ok());
    }

    #[test]
    fn out_of_order_chunks_and_a_changed_id_are_errors() {
        let lines = chunks(&json!({"type": "x", "pad": "y".repeat(100)}), 40, "rpc-3");
        let mut d = Decoder::default();
        assert!(d.push_line(&lines[1]).is_err(), "must start at 0");
        d.push_line(&lines[0]).expect("first");
        assert!(d.push_line(&lines[2]).is_err(), "skipped index 1");
        d.push_line(&lines[0])
            .expect("a new run starts after the error");
        let other = lines[1].replace("rpc-3", "rpc-9");
        assert!(d.push_line(&other).is_err(), "different chunkId");
    }

    #[test]
    fn a_wrong_byte_length_invalid_base64_and_invalid_utf8_are_errors() {
        let mut d = Decoder::default();
        let short = json!({"type": "rpc_chunk", "chunkId": "a", "index": 0, "count": 1,
            "byteLength": 99, "data": STANDARD.encode(b"{}")});
        assert!(d.push_line(&short.to_string()).is_err());
        let long = json!({"type": "rpc_chunk", "chunkId": "a", "index": 0, "count": 1,
            "byteLength": 1, "data": STANDARD.encode(b"{}")});
        assert!(d.push_line(&long.to_string()).is_err());
        let b64 = json!({"type": "rpc_chunk", "chunkId": "a", "index": 0, "count": 1,
            "byteLength": 2, "data": "!!"});
        assert!(d.push_line(&b64.to_string()).is_err());
        let utf8 = json!({"type": "rpc_chunk", "chunkId": "a", "index": 0, "count": 1,
            "byteLength": 2, "data": STANDARD.encode([0xff, 0xfe])});
        assert!(d.push_line(&utf8.to_string()).is_err());
    }

    #[test]
    fn an_object_over_the_limit_is_refused_before_any_bytes_are_kept() {
        let mut d = Decoder::new(10);
        let line = json!({"type": "rpc_chunk", "chunkId": "a", "index": 0, "count": 2,
            "byteLength": 11, "data": STANDARD.encode(b"{}")});
        assert!(d.push_line(&line.to_string()).is_err());
        assert!(d.partial.is_none());
    }

    #[test]
    fn a_line_that_is_not_json_is_an_error_not_a_panic() {
        assert!(matches!(
            Decoder::default().push_line("not json"),
            Err(OmpError::Json(_))
        ));
    }
}
