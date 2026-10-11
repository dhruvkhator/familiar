//! The live browser view's fast path, from the desktop app's own API: frames as they happen
//! (`GET /api/bots/{id}/live/stream`) and the sign-in window (`/api/bots/{id}/live/window`). A hosted API answers 503
//! to both; callers then use [`Client::live_frame`] on `live_frames` notices.

use crate::client::Client;
use crate::error::ApiError;
use crate::types::LiveInput;
use bytes::{Buf, Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

/// One picture of the live view.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveFrame {
    /// Increases with every frame of this teammate's view.
    pub seq: u64,
    /// The page's viewport in CSS pixels (take-over coordinates use these; the picture may be smaller).
    pub view_width: u32,
    pub view_height: u32,
    pub url: String,
    pub title: String,
    pub jpeg: Bytes,
}

/// The teammate's sign-in window: `available` = this API can open one (it runs in the desktop app).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LiveWindow {
    pub available: bool,
    pub open: bool,
}

const MAX_HEADER: usize = 64 * 1024;
const MAX_PICTURE: usize = 32 * 1024 * 1024;

/// Incremental parser for the frame stream: (u32 BE length, JSON header, u32 BE length, JPEG)*. Pings are dropped.
#[derive(Default)]
pub struct FrameParser {
    buf: BytesMut,
}

impl FrameParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every complete frame in what arrived so far; an error when the stream is not a frame stream.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<LiveFrame>, String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 4 {
                return Ok(out);
            }
            let head = u32::from_be_bytes(self.buf[..4].try_into().unwrap()) as usize;
            if head > MAX_HEADER {
                return Err(format!("frame header too long ({head} bytes)"));
            }
            if self.buf.len() < 4 + head + 4 {
                return Ok(out);
            }
            let pic = u32::from_be_bytes(self.buf[4 + head..8 + head].try_into().unwrap()) as usize;
            if pic > MAX_PICTURE {
                return Err(format!("frame too big ({pic} bytes)"));
            }
            if self.buf.len() < 8 + head + pic {
                return Ok(out);
            }
            self.buf.advance(4);
            let header: Value = serde_json::from_slice(&self.buf.split_to(head)).map_err(|e| format!("bad frame header: {e}"))?;
            self.buf.advance(4);
            let jpeg = self.buf.split_to(pic).freeze();
            if header["ping"] == true {
                continue;
            }
            let n = |k: &str| header[k].as_u64().unwrap_or(0);
            let s = |k: &str| header[k].as_str().unwrap_or_default().to_owned();
            out.push(LiveFrame {
                seq: n("seq"),
                view_width: n("vw") as u32,
                view_height: n("vh") as u32,
                url: s("url"),
                title: s("title"),
                jpeg,
            });
        }
    }
}

/// Take-over input that piled up while the previous one was on its way, merged without changing what happens:
/// pointer moves keep only the newest (as noVNC does), wheel turns add up, typed text joins (up to the API's 2000
/// characters). Clicks, keys and navigation stay as they are, in order.
pub fn coalesce_inputs(batch: Vec<LiveInput>) -> Vec<LiveInput> {
    let mut out: Vec<LiveInput> = Vec::with_capacity(batch.len());
    for i in batch {
        if let Some(last) = out.last_mut()
            && last.kind == i.kind
        {
            match i.kind.as_str() {
                "move" => {
                    *last = i;
                    continue;
                }
                "scroll" => {
                    last.dy = Some(last.dy.unwrap_or(0) + i.dy.unwrap_or(0));
                    (last.x, last.y) = (i.x, i.y);
                    continue;
                }
                "type" => {
                    let (a, b) = (last.text.as_deref().unwrap_or_default(), i.text.as_deref().unwrap_or_default());
                    if a.chars().count() + b.chars().count() <= 2000 {
                        last.text = Some(format!("{a}{b}"));
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(i);
    }
    out
}

impl Client {
    /// The live view as it happens: each new frame as soon as the page changes (nothing while it doesn't). `width`×
    /// `height` = the biggest picture you can show, in pixels (0 = the browser's size). Ends when the connection does;
    /// connect again or fall back. Errors: 503 from a hosted API (no desktop app), 404 for someone else's teammate.
    pub async fn live_stream(
        &self,
        bot: Uuid,
        width: u32,
        height: u32,
    ) -> Result<impl Stream<Item = Result<LiveFrame, ApiError>> + Send + 'static, ApiError> {
        let mut req = self.0.stream_http.get(format!("{}/api/bots/{bot}/live/stream?w={width}&h={height}", self.0.base_url));
        if let Some(t) = self.token() {
            req = req.bearer_auth(t);
        }
        let r = req.send().await.map_err(|e| ApiError::Network(e.to_string()))?;
        let status = r.status().as_u16();
        if !(200..300).contains(&status) {
            let body = r.text().await.unwrap_or_default();
            let message = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("Request failed ({status})"));
            return Err(if status == 401 { ApiError::Unauthorized } else { ApiError::Http { status, message } });
        }
        let mut parser = FrameParser::new();
        let frames = r.bytes_stream().flat_map(move |chunk| {
            let items: Vec<Result<LiveFrame, ApiError>> = match chunk {
                Ok(c) => match parser.feed(&c) {
                    Ok(frames) => frames.into_iter().map(Ok).collect(),
                    Err(e) => vec![Err(ApiError::Decode(e))],
                },
                Err(e) => vec![Err(ApiError::Network(e.to_string()))],
            };
            futures_util::stream::iter(items)
        });
        Ok(frames)
    }

    pub async fn live_window(&self, bot: Uuid) -> Result<LiveWindow, ApiError> {
        let (_, b) = self.send(Method::GET, &format!("/api/bots/{bot}/live/window"), None, true).await?;
        serde_json::from_slice(&b).map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// Open the teammate's browser profile in a normal Chrome window on this computer (at `url`, else Chrome's new
    /// tab page), so you sign in to sites in a real browser. 409 with the reason while a run is using the browser.
    pub async fn open_live_window(&self, bot: Uuid, url: Option<&str>) -> Result<(), ApiError> {
        self.mutate::<Value>(Method::POST, &format!("/api/bots/{bot}/live/window"), Some(json!({ "url": url })), true)
            .await
            .map(|_| ())
    }

    /// "Done": close the sign-in window (Chrome saves the session first). True when one was open.
    pub async fn close_live_window(&self, bot: Uuid) -> Result<bool, ApiError> {
        let v: Value = self.mutate(Method::DELETE, &format!("/api/bots/{bot}/live/window"), None, true).await?;
        Ok(v["closed"] == true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(header: Value, jpeg: &[u8]) -> Vec<u8> {
        let h = header.to_string();
        let mut out = (h.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(h.as_bytes());
        out.extend_from_slice(&(jpeg.len() as u32).to_be_bytes());
        out.extend_from_slice(jpeg);
        out
    }

    #[test]
    fn frames_split_anywhere_and_pings_are_dropped() {
        let mut all = message(json!({"seq": 1, "vw": 1280, "vh": 800, "url": "https://a.example/", "title": "A"}), &[0xFF, 0xD8, 1, 2]);
        all.extend(message(json!({"ping": true}), &[]));
        all.extend(message(json!({"seq": 2, "vw": 640, "vh": 400, "url": "https://b.example/", "title": "B"}), &[9; 1000]));
        // Byte by byte, then in odd pieces: the same two frames.
        for step in [1, 3, 7, 1024, all.len()] {
            let mut p = FrameParser::new();
            let mut got = Vec::new();
            for piece in all.chunks(step) {
                got.extend(p.feed(piece).unwrap());
            }
            assert_eq!(got.len(), 2, "step {step}");
            assert_eq!((got[0].seq, got[0].view_width, got[0].url.as_str(), got[0].jpeg.as_ref()), (1, 1280, "https://a.example/", &[0xFF, 0xD8, 1, 2][..]));
            assert_eq!((got[1].seq, got[1].view_height, got[1].title.as_str(), got[1].jpeg.len()), (2, 400, "B", 1000));
        }
    }

    #[test]
    fn piled_up_input_merges_without_reordering() {
        let batch = vec![
            LiveInput::moved(1, 1),
            LiveInput::moved(5, 6),
            LiveInput::click(5, 6),
            LiveInput::type_text("he"),
            LiveInput::type_text("llo"),
            LiveInput::key("Enter"),
            LiveInput { x: Some(1), y: Some(2), ..LiveInput::scroll(40) },
            LiveInput { x: Some(3), y: Some(4), ..LiveInput::scroll(-100) },
            LiveInput::type_text("x".repeat(1999)),
            LiveInput::type_text("yy"),
        ];
        let out = coalesce_inputs(batch);
        let kinds: Vec<&str> = out.iter().map(|i| i.kind.as_str()).collect();
        assert_eq!(kinds, ["move", "click", "type", "key", "scroll", "type", "type"]);
        assert_eq!((out[0].x, out[0].y), (Some(5), Some(6)), "the newest position");
        assert_eq!(out[2].text.as_deref(), Some("hello"));
        assert_eq!((out[4].dy, out[4].x, out[4].y), (Some(-60), Some(3), Some(4)));
        assert_eq!(out[6].text.as_deref(), Some("yy"), "past 2000 characters it starts a new one");
    }

    #[test]
    fn garbage_is_an_error_not_a_hang() {
        let mut p = FrameParser::new();
        assert!(p.feed(b"<html>not a frame stream</html>").is_err());
        let mut p = FrameParser::new();
        let mut bad = message(json!({"seq": 1}), &[]);
        bad[4] = b'{';
        bad[5] = b'{';
        assert!(p.feed(&bad).is_err());
    }
}
