pub fn stream_sse_reader_emit<R: std::io::Read>(
    reader: &mut R,
    limits: TransportLimits,
    protocol: ChatProtocol,
    cancel: &fluxdb_core::CancelFlag,
    emit: &mut dyn FnMut(&fluxdb_core::ModelEvent),
) -> Result<Vec<fluxdb_core::ModelEvent>, TransportError> {
    let mut events = Vec::new();
    read_sse_events(reader, limits, protocol, cancel, &mut |event| {
        emit(event);
        events.push(event.clone());
    })?;
    Ok(events)
}

fn read_sse_events<R: std::io::Read>(
    reader: &mut R,
    limits: TransportLimits,
    protocol: ChatProtocol,
    cancel: &fluxdb_core::CancelFlag,
    emit: &mut dyn FnMut(&fluxdb_core::ModelEvent),
) -> Result<(), TransportError> {
    let mut parser = SseParser::new(limits.max_frame_bytes);
    let mut state = StreamState::default();
    let mut total = 0usize;
    let mut chunk = [0u8; 8192];
    loop {
        if cancel.is_cancelled() {
            // 保留取消前已经解析出的内容（UI 需要显示已流出的部分），再补一个取消结束事件。
            emit(&fluxdb_core::ModelEvent::Finished(
                fluxdb_core::FinishReason::Cancelled,
            ));
            return Ok(());
        }
        let read = reader.read(&mut chunk).map_err(|e| {
            if is_read_timeout(&e) {
                // 正文读取超时同样是瞬时故障：上层可重试，不能降级成普通网络错误。
                TransportError::Timeout(sanitize_io_error(&e))
            } else {
                TransportError::Network(sanitize_io_error(&e))
            }
        })?;
        if read == 0 {
            break;
        }
        total += read;
        if total > limits.max_total_bytes {
            return Err(TransportError::BodyTooLarge {
                limit: limits.max_total_bytes,
            });
        }
        for payload in parser.push(&chunk[..read])? {
            if payload.trim() == "[DONE]" {
                parser.finish();
                return Ok(());
            }
            let parsed = parse_sse_payload(protocol, &payload, &mut state)?;
            for event in &parsed {
                emit(event);
            }
        }
    }
    parser.finish();
    Ok(())
}
