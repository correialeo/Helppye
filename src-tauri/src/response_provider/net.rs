//! Utilidades de streaming HTTP compartilhadas entre providers. Sem depender de crates
//! de parsing SSE: transforma um stream bruto de bytes em um stream de linhas completas
//! (`line_stream`) e, sobre isso, extrai payloads `data: ...` de Server-Sent Events
//! (`sse_data_payloads`). Ollama usa `line_stream` diretamente (NDJSON); OpenAI-compatível
//! e Anthropic usam as duas em conjunto (SSE).

use bytes::Bytes;
use futures_util::stream::{BoxStream, StreamExt};

struct LineSplitterState<E> {
    source: BoxStream<'static, Result<Bytes, E>>,
    buffer: String,
    ended: bool,
}

pub fn line_stream<E: Send + 'static>(
    source: BoxStream<'static, Result<Bytes, E>>,
) -> BoxStream<'static, Result<String, E>> {
    let state = LineSplitterState {
        source,
        buffer: String::new(),
        ended: false,
    };

    Box::pin(futures_util::stream::unfold(
        state,
        |mut state| async move {
            loop {
                if let Some(pos) = state.buffer.find('\n') {
                    let line: String = state.buffer.drain(..=pos).collect();
                    let line = line.trim_end_matches(['\r', '\n']).to_string();
                    return Some((Ok(line), state));
                }

                if state.ended {
                    if state.buffer.is_empty() {
                        return None;
                    }
                    let line = std::mem::take(&mut state.buffer);
                    return Some((Ok(line), state));
                }

                match state.source.next().await {
                    Some(Ok(chunk)) => state.buffer.push_str(&String::from_utf8_lossy(&chunk)),
                    Some(Err(e)) => return Some((Err(e), state)),
                    None => state.ended = true,
                }
            }
        },
    ))
}

pub fn sse_data_payloads<E: Send + 'static>(
    lines: BoxStream<'static, Result<String, E>>,
) -> BoxStream<'static, Result<String, E>> {
    Box::pin(lines.filter_map(|line| async move {
        match line {
            Ok(text) => text
                .strip_prefix("data:")
                .map(|rest| Ok(rest.trim_start().to_string())),
            Err(e) => Some(Err(e)),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    fn bytes_stream(chunks: Vec<&'static str>) -> BoxStream<'static, Result<Bytes, String>> {
        stream::iter(chunks.into_iter().map(|c| Ok(Bytes::from(c.to_string())))).boxed()
    }

    async fn collect_lines(s: BoxStream<'static, Result<String, String>>) -> Vec<String> {
        s.map(|r| r.unwrap()).collect().await
    }

    #[tokio::test]
    async fn splits_single_chunk_into_lines() {
        let lines = collect_lines(line_stream(bytes_stream(vec!["a\nb\nc"]))).await;
        assert_eq!(lines, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn joins_line_split_across_chunks() {
        let lines = collect_lines(line_stream(bytes_stream(vec!["ab", "c\nd", "ef\n"]))).await;
        assert_eq!(lines, vec!["abc", "def"]);
    }

    #[tokio::test]
    async fn strips_carriage_return() {
        let lines = collect_lines(line_stream(bytes_stream(vec!["a\r\nb"]))).await;
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn flushes_trailing_data_without_final_newline() {
        let lines = collect_lines(line_stream(bytes_stream(vec!["a\nb"]))).await;
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn empty_source_yields_no_lines() {
        let lines = collect_lines(line_stream(bytes_stream(vec![]))).await;
        assert!(lines.is_empty());
    }

    #[tokio::test]
    async fn extracts_sse_data_payloads_and_skips_other_lines() {
        let lines = line_stream(bytes_stream(vec![
            "event: message\ndata: {\"a\":1}\n\ndata: [DONE]\n",
        ]));
        let payloads = collect_lines(sse_data_payloads(lines)).await;
        assert_eq!(payloads, vec!["{\"a\":1}", "[DONE]"]);
    }
}
