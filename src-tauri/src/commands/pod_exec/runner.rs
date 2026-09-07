use crate::models::{
    AppError, AppErrorKind, PodExecSessionMessage, PodExecSessionRequest, PodExecSessionSummary,
};
use futures_util::SinkExt;
use k8s_openapi::{api::core::v1::Pod, apimachinery::pkg::apis::meta::v1::Status};
use kube::api::{Api, AttachParams, AttachedProcess, TerminalSize};
use tauri::ipc::Channel;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    task::JoinSet,
};

use super::registry::{
    session_summary, ExecCommand, PodExecRegistry, COMMAND_CAPACITY, INPUT_CHUNK_BYTES,
};
use super::validation::{client_for_context, validate_request, ValidatedPodExecRequest};

fn send(channel: &Channel<PodExecSessionMessage>, message: PodExecSessionMessage) -> bool {
    channel.send(message).is_ok()
}

fn exit_code_from_status(status: &Status) -> Option<i32> {
    status.details.as_ref().and_then(|details| {
        details.causes.as_ref().and_then(|causes| {
            causes.iter().find_map(|cause| {
                if cause.reason.as_deref() == Some("ExitCode") {
                    cause.message.as_ref()?.parse::<i32>().ok()
                } else {
                    None
                }
            })
        })
    })
}

/// Decodes the longest valid UTF-8 prefix out of `pending`, draining consumed
/// bytes. Bytes belonging to a not-yet-complete multibyte sequence stay
/// buffered for the next read; genuinely invalid bytes become one U+FFFD.
fn take_utf8_prefix(pending: &mut Vec<u8>) -> Option<String> {
    let (valid_up_to, invalid_len) = match std::str::from_utf8(pending) {
        Ok(_) => (pending.len(), 0),
        Err(err) => (err.valid_up_to(), err.error_len().unwrap_or(0)),
    };
    if valid_up_to == 0 && invalid_len == 0 {
        return None;
    }
    let mut data = String::from_utf8_lossy(&pending[..valid_up_to]).into_owned();
    if invalid_len > 0 {
        data.push('\u{FFFD}');
    }
    pending.drain(..valid_up_to + invalid_len);
    Some(data)
}

async fn read_exec_output(
    session_id: String,
    stream: &'static str,
    mut reader: impl AsyncRead + Unpin,
    channel: Channel<PodExecSessionMessage>,
) -> Result<(), AppError> {
    let mut buffer = [0_u8; 4096];
    let mut pending = Vec::new();
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => {
                if !pending.is_empty() {
                    let data = String::from_utf8_lossy(&pending).into_owned();
                    send(
                        &channel,
                        PodExecSessionMessage::Output {
                            session_id: session_id.clone(),
                            stream: stream.to_string(),
                            data,
                        },
                    );
                }
                break;
            }
            Ok(size) => {
                pending.extend_from_slice(&buffer[..size]);
                while let Some(data) = take_utf8_prefix(&mut pending) {
                    if !send(
                        &channel,
                        PodExecSessionMessage::Output {
                            session_id: session_id.clone(),
                            stream: stream.to_string(),
                            data,
                        },
                    ) {
                        return Err(AppError::new(
                            "exec output channel closed",
                            AppErrorKind::Session,
                        ));
                    }
                }
            }
            Err(error) => {
                return Err(
                    AppError::new("Could not read exec output", AppErrorKind::Io)
                        .with_source(error),
                )
            }
        }
    }
    Ok(())
}

async fn run_exec_session(
    summary: PodExecSessionSummary,
    request: ValidatedPodExecRequest,
    commands: mpsc::Receiver<ExecCommand>,
    channel: Channel<PodExecSessionMessage>,
    registry: PodExecRegistry,
) {
    let session_id = summary.id.clone();
    send(
        &channel,
        PodExecSessionMessage::Status {
            session_id: session_id.clone(),
            status: "connecting".to_string(),
            message: "Opening Kubernetes exec session".to_string(),
        },
    );

    let client = match client_for_context(
        &request.cluster_context,
        request.kubeconfig_env_var.clone(),
    )
    .await
    {
        Ok(client) => client,
        Err(err) => {
            registry.mark_error(&session_id, err.message.clone());
            send(
                &channel,
                PodExecSessionMessage::Error {
                    session_id,
                    message: err.message,
                },
            );
            return;
        }
    };
    let pods: Api<Pod> = Api::namespaced(client, &request.namespace);
    let mut params = AttachParams::default()
        .stdin(request.stdin)
        .stdout(true)
        .stderr(!request.tty)
        .tty(request.tty)
        .max_stdin_buf_size(INPUT_CHUNK_BYTES)
        .max_stdout_buf_size(64 * 1024)
        .max_stderr_buf_size(64 * 1024);
    if let Some(container) = &request.container {
        params = params.container(container.clone());
    }

    let mut attached = match pods
        .exec(&request.pod_name, request.command.clone(), &params)
        .await
    {
        Ok(attached) => attached,
        Err(err) => {
            let message = err.to_string();
            registry.mark_error(&session_id, message.clone());
            send(
                &channel,
                PodExecSessionMessage::Error {
                    session_id,
                    message,
                },
            );
            return;
        }
    };

    registry.mark_running(&session_id);
    send(
        &channel,
        PodExecSessionMessage::Status {
            session_id: session_id.clone(),
            status: "running".to_string(),
            message: "Exec session is running".to_string(),
        },
    );

    let result = drive_exec_io(
        &mut attached,
        &session_id,
        &request,
        commands,
        &channel,
        &registry,
    )
    .await;
    // AttachedProcess owns and aborts its WebSocket task on drop, including errors.
    drop(attached);
    match result {
        Ok(status) => {
            let exit_code = exit_code_from_status(&status);
            registry.mark_exited(&session_id, exit_code);
            send(
                &channel,
                PodExecSessionMessage::Exited {
                    session_id: session_id.clone(),
                    exit_code,
                    reason: status.reason,
                    message: status.message,
                },
            );
        }
        Err(error) => {
            registry.mark_error(&session_id, error.message.clone());
            send(
                &channel,
                PodExecSessionMessage::Error {
                    session_id: session_id.clone(),
                    message: error.message,
                },
            );
        }
    }
    send(&channel, PodExecSessionMessage::Stopped { session_id });
}

async fn drive_exec_io(
    attached: &mut AttachedProcess,
    session_id: &str,
    request: &ValidatedPodExecRequest,
    mut commands: mpsc::Receiver<ExecCommand>,
    channel: &Channel<PodExecSessionMessage>,
    registry: &PodExecRegistry,
) -> Result<Status, AppError> {
    let mut readers = JoinSet::new();
    if let Some(stdout) = attached.stdout() {
        readers.spawn(read_exec_output(
            session_id.to_string(),
            if request.tty { "terminal" } else { "stdout" },
            stdout,
            channel.clone(),
        ));
    }
    if let Some(stderr) = attached.stderr() {
        readers.spawn(read_exec_output(
            session_id.to_string(),
            "stderr",
            stderr,
            channel.clone(),
        ));
    }
    let status = attached.take_status().ok_or_else(|| {
        AppError::new("exec status receiver is unavailable", AppErrorKind::Session)
    })?;
    let mut stdin = attached.stdin();
    let mut terminal_size = attached.terminal_size();
    let status = {
        let input = async {
            if let Some(sender) = terminal_size.as_mut() {
                sender
                    .send(TerminalSize {
                        width: request.terminal_size.cols,
                        height: request.terminal_size.rows,
                    })
                    .await
                    .map_err(|error| {
                        AppError::new("Could not resize exec terminal", AppErrorKind::Session)
                            .with_source(error)
                    })?;
            }
            while let Some(command) = commands.recv().await {
                match command {
                    ExecCommand::Stdin {
                        data,
                        acknowledged,
                        _permit,
                    } => {
                        let result = match stdin.as_mut() {
                            Some(writer) => writer.write_all(&data).await.map_err(|error| {
                                AppError::new("Could not write exec input", AppErrorKind::Io)
                                    .with_source(error)
                            }),
                            None => Err(AppError::new(
                                "exec session stdin is unavailable",
                                AppErrorKind::Session,
                            )),
                        };
                        let _ = acknowledged.send(result.clone());
                        result?;
                    }
                    ExecCommand::Resize(size) => {
                        if let Some(sender) = terminal_size.as_mut() {
                            sender
                                .send(TerminalSize {
                                    width: size.cols,
                                    height: size.rows,
                                })
                                .await
                                .map_err(|error| {
                                    AppError::new(
                                        "Could not resize exec terminal",
                                        AppErrorKind::Session,
                                    )
                                    .with_source(error)
                                })?;
                            registry.mark_terminal_size(session_id, size);
                        }
                    }
                }
            }
            Err::<(), AppError>(AppError::new(
                "exec input channel closed before completion",
                AppErrorKind::Session,
            ))
        };
        tokio::pin!(input, status);
        loop {
            tokio::select! {
                status = &mut status => break status.ok_or_else(|| AppError::new("exec session closed without an exit status", AppErrorKind::Session))?,
                result = &mut input => { result?; unreachable!("input loop returns only on error"); },
                result = readers.join_next(), if !readers.is_empty() => {
                    result.expect("readers is not empty")
                        .map_err(|error| AppError::new("exec output task failed", AppErrorKind::Internal).with_source(error))??;
                }
            }
        }
    };
    drop(stdin);
    drop(terminal_size);
    // Deliver all buffered output before Exited closes the frontend channel.
    while let Some(result) = readers.join_next().await {
        result.map_err(|error| {
            AppError::new("exec output task failed", AppErrorKind::Internal).with_source(error)
        })??;
    }
    Ok(status)
}

pub(super) async fn start_pod_exec_session_in_registry(
    request: PodExecSessionRequest,
    channel: Channel<PodExecSessionMessage>,
    registry: &PodExecRegistry,
) -> Result<PodExecSessionSummary, AppError> {
    let request = validate_request(&request)?;
    let session_id = registry.session_id();
    let summary = session_summary(session_id.clone(), &request);
    let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let summary = registry.insert(summary, commands);
    send(
        &channel,
        PodExecSessionMessage::Started {
            session_id: session_id.clone(),
            summary: summary.clone(),
        },
    );
    let registry_for_task = registry.clone();
    let handle = tauri::async_runtime::spawn(run_exec_session(
        summary.clone(),
        request,
        command_rx,
        channel.clone(),
        registry_for_task,
    ));
    registry.set_handle(&session_id, handle);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::take_utf8_prefix;

    #[test]
    fn keeps_incomplete_multibyte_tail_for_next_chunk() {
        let mut pending = "héllo".as_bytes().to_vec();
        let split = 2; // 'h' plus the first byte of 'é'
        let mut remainder = pending.split_off(split);

        let first = take_utf8_prefix(&mut pending).expect("first chunk");
        assert_eq!(first, "h");
        assert!(!pending.is_empty());

        pending.append(&mut remainder);
        assert_eq!(take_utf8_prefix(&mut pending).expect("rest"), "éllo");
        assert!(pending.is_empty());
    }

    #[test]
    fn replaces_invalid_bytes_with_replacement_char() {
        let mut pending = vec![b'a', 0xFF, b'b'];

        assert_eq!(take_utf8_prefix(&mut pending).expect("chunk"), "a\u{FFFD}");
        assert_eq!(take_utf8_prefix(&mut pending).expect("rest"), "b");
        assert!(pending.is_empty());
    }

    #[test]
    fn buffers_entire_incomplete_sequence() {
        let mut pending = "é".as_bytes().to_vec()[..1].to_vec();

        assert!(take_utf8_prefix(&mut pending).is_none());
        assert_eq!(pending.len(), 1);

        pending.push(0xA9);
        assert_eq!(take_utf8_prefix(&mut pending).expect("complete"), "é");
        assert!(pending.is_empty());
    }
}
