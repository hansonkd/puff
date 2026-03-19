//! SSE stream parser for LLM provider responses.

#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event_type: Option<String>,
    pub data: String,
}

pub struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        SseParser { buffer: Vec::new() }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);

        let mut events = Vec::new();

        loop {
            // Find the next double-newline delimiter (\n\n or \r\n\r\n)
            let delimiter_pos = self.find_event_delimiter();
            match delimiter_pos {
                None => break,
                Some((start, end)) => {
                    let event_bytes = self.buffer[..start].to_vec();
                    let remaining = self.buffer[end..].to_vec();
                    self.buffer = remaining;

                    if let Some(event) = Self::parse_event(&event_bytes) {
                        events.push(event);
                    }
                }
            }
        }

        events
    }

    /// Find the position of the next event delimiter (\n\n or \r\n\r\n).
    /// Returns (content_end, delimiter_end) so the caller knows both where
    /// the event content ends and where to resume reading.
    fn find_event_delimiter(&self) -> Option<(usize, usize)> {
        let buf = &self.buffer;
        let len = buf.len();

        let mut i = 0;
        while i < len {
            // Check for \r\n\r\n
            if i + 3 < len
                && buf[i] == b'\r'
                && buf[i + 1] == b'\n'
                && buf[i + 2] == b'\r'
                && buf[i + 3] == b'\n'
            {
                return Some((i, i + 4));
            }
            // Check for \n\n
            if i + 1 < len && buf[i] == b'\n' && buf[i + 1] == b'\n' {
                return Some((i, i + 2));
            }
            i += 1;
        }
        None
    }

    fn parse_event(raw: &[u8]) -> Option<SseEvent> {
        let text = std::str::from_utf8(raw).ok()?;

        let mut event_type: Option<String> = None;
        let mut data_lines: Vec<&str> = Vec::new();

        for line in text.lines() {
            if let Some(value) = line.strip_prefix("data: ") {
                data_lines.push(value);
            } else if let Some(value) = line.strip_prefix("event: ") {
                event_type = Some(value.to_string());
            }
            // Ignore comment lines (starting with ':') and unknown fields
        }

        if data_lines.is_empty() && event_type.is_none() {
            return None;
        }

        Some(SseEvent {
            event_type,
            data: data_lines.join("\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse_data_line() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: {\"type\":\"content\"}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\"type\":\"content\"}");
    }

    #[test]
    fn test_parse_sse_multi_line() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: hello\ndata: world\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello\nworld");
    }

    #[test]
    fn test_parse_sse_with_event_type() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"event: message_start\ndata: {}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type.as_deref(), Some("message_start"));
    }

    #[test]
    fn test_parse_sse_partial_then_complete() {
        let mut parser = SseParser::new();
        let events1 = parser.feed(b"data: hel");
        assert!(events1.is_empty());
        let events2 = parser.feed(b"lo\n\n");
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].data, "hello");
    }

    #[test]
    fn test_parse_sse_done_signal() {
        let mut parser = SseParser::new();
        let events = parser.feed(b"data: [DONE]\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "[DONE]");
    }
}
