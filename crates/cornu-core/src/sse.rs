use std::time::{Duration, Instant};

use crate::http::response_headers;
use crate::{HttpClient, Request, Response, SendError};

pub fn is_event_stream(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
    /// The last id the stream set, as `EventSource.lastEventId`.
    pub id: Option<String>,
    pub retry: Option<u64>,
}

/// Follows the "interpreting an event stream" algorithm of the WHATWG HTML standard.
#[derive(Debug, Default)]
pub struct SseParser {
    line: Vec<u8>,
    after_cr: bool,
    started: bool,
    event: String,
    data: String,
    id: Option<String>,
    retry: Option<u64>,
}

impl SseParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        let mut events = Vec::new();
        for &byte in bytes {
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                continue;
            }
            if byte == b'\n' || byte == b'\r' {
                self.after_cr = byte == b'\r';
                let line = std::mem::take(&mut self.line);
                if let Some(event) = self.line(&line) {
                    events.push(event);
                }
            } else {
                self.line.push(byte);
            }
        }
        events
    }

    fn line(&mut self, mut line: &[u8]) -> Option<SseEvent> {
        if !std::mem::replace(&mut self.started, true) {
            line = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line);
        }
        if line.is_empty() {
            return self.dispatch();
        }
        let line = String::from_utf8_lossy(line);
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (&*line, ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
            }
            "id" if !value.contains('\0') => {
                self.id = (!value.is_empty()).then(|| value.to_owned());
            }
            "retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                self.retry = value.parse().ok();
            }
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = std::mem::take(&mut self.event);
        let retry = self.retry.take();
        if self.data.is_empty() {
            return None;
        }
        let mut data = std::mem::take(&mut self.data);
        data.pop();
        Some(SseEvent {
            event: if event.is_empty() {
                "message".to_owned()
            } else {
                event
            },
            data,
            id: self.id.clone(),
            retry,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Streamed {
    /// The response is an event stream; its body follows as events.
    Head(Response),
    Event(SseEvent),
}

impl HttpClient {
    /// Like `send`, but an event stream is reported as it arrives and is not subject to the
    /// timeout. It ends when the server closes it or `stop` completes, with the body received.
    pub async fn send_streaming(
        &self,
        request: &Request,
        stop: impl Future<Output = ()>,
        mut on_update: impl FnMut(Streamed),
    ) -> Result<Response, SendError> {
        // reqwest cannot lift the client timeout for one request: it is pushed back and
        // applied here instead, to everything but an event stream.
        let builder = self
            .prepare(request)
            .await?
            .timeout(Duration::from_secs(365 * 24 * 3600));
        let deadline = self
            .options
            .timeout
            .map(|timeout| tokio::time::Instant::now() + timeout);
        let started = Instant::now();
        let mut response = within(deadline, builder.send()).await??;
        let status = response.status().as_u16();
        let headers = response_headers(&response);
        let streaming = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(is_event_stream);
        if !streaming {
            let body = within(deadline, response.bytes()).await??.to_vec();
            return Ok(Response {
                status,
                headers,
                body,
                elapsed: started.elapsed(),
            });
        }

        on_update(Streamed::Head(Response {
            status,
            headers: headers.clone(),
            body: Vec::new(),
            elapsed: started.elapsed(),
        }));
        let mut parser = SseParser::default();
        let mut body = Vec::new();
        let mut stop = std::pin::pin!(stop);
        loop {
            tokio::select! {
                chunk = response.chunk() => match chunk? {
                    Some(bytes) => {
                        body.extend_from_slice(&bytes);
                        for event in parser.feed(&bytes) {
                            on_update(Streamed::Event(event));
                        }
                    }
                    None => break,
                },
                () = &mut stop => break,
            }
        }
        Ok(Response {
            status,
            headers,
            body,
            elapsed: started.elapsed(),
        })
    }
}

pub(crate) async fn within<F: Future>(
    deadline: Option<tokio::time::Instant>,
    future: F,
) -> Result<F::Output, SendError> {
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, future)
            .await
            .map_err(|_| SendError::Timeout),
        None => Ok(future.await),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut parser = SseParser::default();
        chunks.iter().flat_map(|chunk| parser.feed(chunk)).collect()
    }

    fn event(event: &str, data: &str, id: Option<&str>) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
            id: id.map(Into::into),
            retry: None,
        }
    }

    #[test]
    fn content_type_detection() {
        assert!(is_event_stream("text/event-stream"));
        assert!(is_event_stream("Text/Event-Stream; charset=utf-8"));
        assert!(!is_event_stream("application/json"));
    }

    #[test]
    fn fields_comments_and_multiline_data() {
        let events = parse(&[
            b"\xef\xbb\xbf: welcome\n\ndata: first\ndata:  two spaces\n\n",
            b"event: update\nid: 7\ndata\n\n",
            b"data: {\"a\":1}\n\n",
        ]);
        assert_eq!(
            events,
            [
                event("message", "first\n two spaces", None),
                event("update", "", Some("7")),
                event("message", "{\"a\":1}", Some("7")),
            ]
        );
    }

    #[test]
    fn every_line_ending_and_split_chunks() {
        let events = parse(&[
            b"data: a\r",
            b"\n\r",
            b"\ndata: b\rdata: \xc3",
            b"\xa9t\xc3\xa9\r\rdata: c",
            b"\n",
            b"\n",
        ]);
        assert_eq!(
            events,
            [
                event("message", "a", None),
                event("message", "b\nété", None),
                event("message", "c", None),
            ]
        );
    }

    #[test]
    fn events_without_data_ids_and_retry() {
        let events = parse(&[
            b"event: ignored\n\nretry: 3000\nretry: soon\nid: a\0b\ndata: x\n\n",
            b"id\ndata: y\n\ndata: unterminated",
        ]);
        assert_eq!(
            events,
            [
                SseEvent {
                    retry: Some(3000),
                    ..event("message", "x", None)
                },
                event("message", "y", None),
            ]
        );
    }
}
