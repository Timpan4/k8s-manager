use super::{client_for_context, send};
use crate::models::{LogLineSource, PodLogStreamRequest, StreamMessage};
use futures_util::{io::AsyncBufRead, AsyncBufReadExt};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, LogParams};
use tauri::ipc::Channel;

pub(super) async fn run_pod_log_stream(
    stream_id: String,
    request: PodLogStreamRequest,
    channel: Channel<StreamMessage>,
) {
    let client = match client_for_context(
        &request.cluster_context,
        request.kubeconfig_env_var.clone(),
    )
    .await
    {
        Ok(client) => client,
        Err(err) => {
            send(
                &channel,
                StreamMessage::Error {
                    stream_id,
                    message: err.message,
                },
            );
            return;
        }
    };
    let pods: Api<Pod> = Api::namespaced(client, &request.namespace);
    let params = LogParams {
        container: request.container.clone(),
        follow: true,
        tail_lines: Some(request.tail_lines.unwrap_or(200)),
        since_seconds: request.since_seconds,
        timestamps: true,
        ..LogParams::default()
    };

    if !send(
        &channel,
        StreamMessage::Status {
            stream_id: stream_id.clone(),
            status: "connected".to_string(),
            message: "Streaming logs".to_string(),
        },
    ) {
        return;
    }

    match pods.log_stream(&request.pod_name, &params).await {
        Ok(logs) => {
            let mut logs = logs;
            loop {
                match read_log_line(&mut logs).await {
                    Ok(Some(line)) => {
                        if !send(
                            &channel,
                            StreamMessage::LogLine {
                                stream_id: stream_id.clone(),
                                line,
                                source: Some(LogLineSource {
                                    pod_name: request.pod_name.clone(),
                                    container: request.container.clone(),
                                }),
                            },
                        ) {
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        send(
                            &channel,
                            StreamMessage::Error {
                                stream_id: stream_id.clone(),
                                message: err.to_string(),
                            },
                        );
                        break;
                    }
                }
            }
        }
        Err(err) => {
            send(
                &channel,
                StreamMessage::Error {
                    stream_id: stream_id.clone(),
                    message: err.to_string(),
                },
            );
        }
    }

    send(&channel, StreamMessage::Stopped { stream_id });
}

// Product budget approved for a single log line; it excludes the truncation notice.
pub(super) const MAX_LOG_LINE_BYTES: usize = 256 * 1024;

pub(super) async fn read_log_line(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut saw_bytes = false;
    // Keep one extra byte to distinguish an exact-limit CRLF from an oversized line.
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            if !saw_bytes {
                return Ok(None);
            }
            break;
        }
        saw_bytes = true;
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(buffer.len());
        let retained = content_len.min((MAX_LOG_LINE_BYTES + 1).saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= content_len > retained;
        let consumed = content_len + usize::from(newline.is_some());
        reader.consume_unpin(consumed);
        if newline.is_some() {
            if bytes.last() == Some(&b'\r') && !truncated {
                bytes.pop();
            }
            break;
        }
        // A buffered source can stay ready indefinitely while an oversized line is drained.
        tokio::task::yield_now().await;
    }
    truncated |= bytes.len() > MAX_LOG_LINE_BYTES;
    bytes.truncate(MAX_LOG_LINE_BYTES);
    if truncated {
        if let Err(error) = std::str::from_utf8(&bytes) {
            if error.error_len().is_none() {
                bytes.truncate(error.valid_up_to());
            }
        }
    }
    let mut line = String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if truncated {
        line.push_str(" [truncated at 256 KiB]");
    }
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::io::{BufReader, Cursor};

    #[tokio::test]
    async fn oversized_line_is_marked_and_next_line_is_preserved() {
        let mut data = vec![b'x'; MAX_LOG_LINE_BYTES + 1];
        data.extend_from_slice(b"\nnext\n");
        let mut reader = BufReader::new(Cursor::new(data));
        let line = read_log_line(&mut reader).await.unwrap().unwrap();
        assert_eq!(
            line,
            format!("{} [truncated at 256 KiB]", "x".repeat(MAX_LOG_LINE_BYTES))
        );
        assert_eq!(
            read_log_line(&mut reader).await.unwrap().as_deref(),
            Some("next")
        );
        assert_eq!(read_log_line(&mut reader).await.unwrap(), None);
    }

    #[tokio::test]
    async fn exact_limit_and_crlf_do_not_truncate() {
        for ending in ["\n", "\r\n", ""] {
            let line = "x".repeat(MAX_LOG_LINE_BYTES);
            let mut reader = BufReader::new(Cursor::new(format!("{line}{ending}").into_bytes()));
            assert_eq!(read_log_line(&mut reader).await.unwrap(), Some(line));
            assert_eq!(read_log_line(&mut reader).await.unwrap(), None);
        }
    }

    #[tokio::test]
    async fn truncation_preserves_utf8_boundaries_and_empty_lines() {
        let prefix = "x".repeat(MAX_LOG_LINE_BYTES - 1);
        let mut reader = BufReader::new(Cursor::new(format!("{prefix}é\n\nlast").into_bytes()));
        assert_eq!(
            read_log_line(&mut reader).await.unwrap(),
            Some(format!("{prefix} [truncated at 256 KiB]"))
        );
        assert_eq!(
            read_log_line(&mut reader).await.unwrap().as_deref(),
            Some("")
        );
        assert_eq!(
            read_log_line(&mut reader).await.unwrap().as_deref(),
            Some("last")
        );
    }
}
