//! Wire codecs for shared-workspace snapshots and identified topologies.
//!
//! Workspace payloads are exchanged as bounded DTLS datagrams with ACK/retry
//! (see [`ProtoEvent::WorkspaceChunk`] / [`ProtoEvent::WorkspaceAck`]). They are
//! never assumed to fit in a single topology datagram.

use crate::{EventType, ProtoEvent, ProtocolError};
use input_event::display::DisplayRect;
use input_event::workspace::{
    DevicePlacement, DisplayId, LocalDisplay, LocalTopology, MAX_DISPLAY_ID_LEN,
    MAX_LOCAL_DISPLAYS, MAX_WORKSPACE_DEVICES, WorkspaceConfig,
};
/// Wire version for identified topology frames.
pub const IDENTIFIED_TOPOLOGY_VERSION: u8 = 1;

/// Maximum total workspace payload across all chunks.
pub const MAX_WORKSPACE_PAYLOAD: usize = 48 * 1024;

/// Maximum bytes of workspace data in one chunk datagram.
pub const WORKSPACE_CHUNK_DATA: usize = 1024;

/// Maximum number of chunks for one revision.
pub const MAX_WORKSPACE_CHUNKS: usize = MAX_WORKSPACE_PAYLOAD / WORKSPACE_CHUNK_DATA;

/// Maximum identified-topology frame size. Matches the clipboard datagram
/// budget so DTLS peers that already accept 4 KiB frames can carry ids.
pub const MAX_IDENTIFIED_TOPOLOGY_SIZE: usize = 4 * 1024;

const _: () = assert!(MAX_WORKSPACE_CHUNKS <= 255);

/// Encode [`ProtoEvent::IdentifiedTopology`].
pub fn encode_identified_topology(event: &ProtoEvent) -> Result<Vec<u8>, ProtocolError> {
    let (epoch, generation, topology) = match event {
        ProtoEvent::IdentifiedTopology {
            epoch,
            generation,
            topology,
        } => (*epoch, *generation, topology),
        _ => panic!("encode_identified_topology on wrong variant"),
    };
    if topology.displays.len() > MAX_LOCAL_DISPLAYS {
        return Err(ProtocolError::InvalidDisplayRectCount(
            topology.displays.len(),
        ));
    }
    let mut buf = Vec::with_capacity(15);
    buf.push(EventType::IdentifiedTopology as u8);
    buf.push(IDENTIFIED_TOPOLOGY_VERSION);
    buf.extend_from_slice(&epoch.to_be_bytes());
    buf.extend_from_slice(&generation.to_be_bytes());
    buf.push(topology.displays.len() as u8);
    for (index, display) in topology.displays.iter().enumerate() {
        let id = display.id.as_str().as_bytes();
        if id.is_empty() || id.len() > MAX_DISPLAY_ID_LEN {
            return Err(ProtocolError::InvalidDisplayRectangle(index));
        }
        buf.push(id.len() as u8);
        buf.extend_from_slice(id);
        let (x, y) = display.rect.origin();
        let (w, h) = display.rect.size();
        buf.extend_from_slice(&x.to_be_bytes());
        buf.extend_from_slice(&y.to_be_bytes());
        buf.extend_from_slice(&w.to_be_bytes());
        buf.extend_from_slice(&h.to_be_bytes());
        let scale_milli = display
            .scale
            .filter(|s| s.is_finite() && *s > 0.0)
            .map(|s| (s * 1000.0).round() as u32)
            .unwrap_or(0);
        buf.extend_from_slice(&scale_milli.to_be_bytes());
    }
    if buf.len() > MAX_IDENTIFIED_TOPOLOGY_SIZE {
        return Err(ProtocolError::DisplayLayoutTooLarge(buf.len()));
    }
    Ok(buf)
}

/// Decode an identified topology frame.
pub fn decode_identified_topology(buf: &[u8]) -> Result<ProtoEvent, ProtocolError> {
    if buf.len() > MAX_IDENTIFIED_TOPOLOGY_SIZE {
        return Err(ProtocolError::DisplayLayoutTooLarge(buf.len()));
    }
    if buf.len() < 15 {
        return Err(ProtocolError::BufferTooSmall);
    }
    if !matches!(EventType::try_from(buf[0])?, EventType::IdentifiedTopology) {
        return Err(ProtocolError::BufferTooSmall);
    }
    if buf[1] != IDENTIFIED_TOPOLOGY_VERSION {
        return Err(ProtocolError::UnsupportedDisplayLayoutVersion(buf[1]));
    }
    let epoch = u64::from_be_bytes(buf[2..10].try_into().expect("8"));
    let generation = u32::from_be_bytes(buf[10..14].try_into().expect("4"));
    let count = usize::from(buf[14]);
    if count > MAX_LOCAL_DISPLAYS {
        return Err(ProtocolError::InvalidDisplayRectCount(count));
    }
    let mut data = &buf[15..];
    let mut displays = Vec::with_capacity(count);
    for index in 0..count {
        if data.is_empty() {
            return Err(ProtocolError::BufferTooSmall);
        }
        let id_len = usize::from(data[0]);
        data = &data[1..];
        if id_len == 0 || id_len > MAX_DISPLAY_ID_LEN || data.len() < id_len + 20 {
            return Err(ProtocolError::InvalidDisplayRectangle(index));
        }
        let id_str = std::str::from_utf8(&data[..id_len])
            .map_err(|_| ProtocolError::InvalidDisplayRectangle(index))?;
        let id = DisplayId::new(id_str).ok_or(ProtocolError::InvalidDisplayRectangle(index))?;
        data = &data[id_len..];
        let x = i32::from_be_bytes(data[0..4].try_into().expect("4"));
        let y = i32::from_be_bytes(data[4..8].try_into().expect("4"));
        let w = u32::from_be_bytes(data[8..12].try_into().expect("4"));
        let h = u32::from_be_bytes(data[12..16].try_into().expect("4"));
        let scale_milli = u32::from_be_bytes(data[16..20].try_into().expect("4"));
        data = &data[20..];
        let rect =
            DisplayRect::new(x, y, w, h).ok_or(ProtocolError::InvalidDisplayRectangle(index))?;
        let scale = if scale_milli == 0 {
            None
        } else {
            Some(scale_milli as f32 / 1000.0)
        };
        displays.push(LocalDisplay {
            id,
            rect,
            scale,
            rotation: None,
        });
    }
    if !data.is_empty() {
        return Err(ProtocolError::DisplayLayoutTrailingData);
    }
    let topology = LocalTopology::try_new(displays, epoch, generation)
        .map_err(|_| ProtocolError::InvalidDisplayRectCount(count))?;
    Ok(ProtoEvent::IdentifiedTopology {
        epoch,
        generation,
        topology,
    })
}

/// Serialize a workspace config into a bounded binary payload.
pub fn encode_workspace_payload(config: &WorkspaceConfig) -> Result<Vec<u8>, ProtocolError> {
    config
        .validate()
        .map_err(|_| ProtocolError::InvalidWorkspacePayload)?;
    if config.placements.len() > MAX_WORKSPACE_DEVICES {
        return Err(ProtocolError::InvalidWorkspacePayload);
    }
    let mut buf = Vec::new();
    write_str(&mut buf, &config.workspace_id)?;
    buf.extend_from_slice(&config.revision.to_be_bytes());
    write_str(&mut buf, &config.authority_fingerprint)?;
    buf.push(config.placements.len() as u8);
    for placement in &config.placements {
        write_str(&mut buf, &placement.peer_fingerprint)?;
        buf.extend_from_slice(&placement.origin_in_workspace.0.to_be_bytes());
        buf.extend_from_slice(&placement.origin_in_workspace.1.to_be_bytes());
    }
    if buf.len() > MAX_WORKSPACE_PAYLOAD {
        return Err(ProtocolError::WorkspacePayloadTooLarge(buf.len()));
    }
    Ok(buf)
}

/// Deserialize a workspace config payload.
pub fn decode_workspace_payload(mut data: &[u8]) -> Result<WorkspaceConfig, ProtocolError> {
    if data.len() > MAX_WORKSPACE_PAYLOAD {
        return Err(ProtocolError::WorkspacePayloadTooLarge(data.len()));
    }
    let workspace_id = read_str(&mut data)?;
    if data.len() < 8 {
        return Err(ProtocolError::BufferTooSmall);
    }
    let revision = u64::from_be_bytes(data[..8].try_into().expect("8"));
    data = &data[8..];
    let authority_fingerprint = read_str(&mut data)?;
    if data.is_empty() {
        return Err(ProtocolError::BufferTooSmall);
    }
    let count = usize::from(data[0]);
    data = &data[1..];
    if count > MAX_WORKSPACE_DEVICES {
        return Err(ProtocolError::InvalidWorkspacePayload);
    }
    let mut placements = Vec::with_capacity(count);
    for _ in 0..count {
        let peer_fingerprint = read_str(&mut data)?;
        if data.len() < 8 {
            return Err(ProtocolError::BufferTooSmall);
        }
        let ox = i32::from_be_bytes(data[0..4].try_into().expect("4"));
        let oy = i32::from_be_bytes(data[4..8].try_into().expect("4"));
        data = &data[8..];
        placements.push(DevicePlacement {
            peer_fingerprint,
            origin_in_workspace: (ox, oy),
        });
    }
    if !data.is_empty() {
        return Err(ProtocolError::DisplayLayoutTrailingData);
    }
    let config = WorkspaceConfig {
        workspace_id,
        revision,
        authority_fingerprint,
        placements,
    };
    config
        .validate()
        .map_err(|_| ProtocolError::InvalidWorkspacePayload)?;
    Ok(config)
}

/// Split a workspace payload into chunk ProtoEvents.
pub fn workspace_chunks(revision: u64, payload: &[u8]) -> Result<Vec<ProtoEvent>, ProtocolError> {
    if payload.len() > MAX_WORKSPACE_PAYLOAD {
        return Err(ProtocolError::WorkspacePayloadTooLarge(payload.len()));
    }
    if payload.is_empty() {
        return Err(ProtocolError::InvalidWorkspacePayload);
    }
    let count = payload.len().div_ceil(WORKSPACE_CHUNK_DATA);
    if count > MAX_WORKSPACE_CHUNKS {
        return Err(ProtocolError::WorkspacePayloadTooLarge(payload.len()));
    }
    let mut chunks = Vec::with_capacity(count);
    for (idx, piece) in payload.chunks(WORKSPACE_CHUNK_DATA).enumerate() {
        chunks.push(ProtoEvent::WorkspaceChunk {
            revision,
            index: idx as u8,
            count: count as u8,
            data: piece.to_vec(),
        });
    }
    Ok(chunks)
}

/// Encode a workspace chunk datagram.
pub fn encode_workspace_chunk(event: &ProtoEvent) -> Result<Vec<u8>, ProtocolError> {
    let (revision, index, count, data) = match event {
        ProtoEvent::WorkspaceChunk {
            revision,
            index,
            count,
            data,
        } => (*revision, *index, *count, data),
        _ => panic!("encode_workspace_chunk on wrong variant"),
    };
    if count == 0 || usize::from(count) > MAX_WORKSPACE_CHUNKS || index >= count {
        return Err(ProtocolError::InvalidWorkspacePayload);
    }
    if data.len() > WORKSPACE_CHUNK_DATA {
        return Err(ProtocolError::WorkspacePayloadTooLarge(data.len()));
    }
    let mut buf = Vec::with_capacity(12 + data.len());
    buf.push(EventType::WorkspaceChunk as u8);
    buf.extend_from_slice(&revision.to_be_bytes());
    buf.push(index);
    buf.push(count);
    buf.extend_from_slice(&(data.len() as u16).to_be_bytes());
    buf.extend_from_slice(data);
    Ok(buf)
}

/// Decode a workspace chunk datagram.
pub fn decode_workspace_chunk(buf: &[u8]) -> Result<ProtoEvent, ProtocolError> {
    if buf.len() < 13 {
        return Err(ProtocolError::BufferTooSmall);
    }
    if !matches!(EventType::try_from(buf[0])?, EventType::WorkspaceChunk) {
        return Err(ProtocolError::BufferTooSmall);
    }
    let revision = u64::from_be_bytes(buf[1..9].try_into().expect("8"));
    let index = buf[9];
    let count = buf[10];
    let data_len = u16::from_be_bytes([buf[11], buf[12]]) as usize;
    if data_len > WORKSPACE_CHUNK_DATA || buf.len() != 13 + data_len {
        return Err(ProtocolError::InvalidWorkspacePayload);
    }
    if count == 0 || usize::from(count) > MAX_WORKSPACE_CHUNKS || index >= count {
        return Err(ProtocolError::InvalidWorkspacePayload);
    }
    Ok(ProtoEvent::WorkspaceChunk {
        revision,
        index,
        count,
        data: buf[13..].to_vec(),
    })
}

/// Reassemble chunk data for one revision. Returns `None` until all chunks arrive.
pub fn try_reassemble_workspace(
    chunks: &[(u8, u8, Vec<u8>)],
) -> Result<Option<Vec<u8>>, ProtocolError> {
    if chunks.is_empty() {
        return Ok(None);
    }
    let count = chunks[0].1;
    if count == 0 || usize::from(count) > MAX_WORKSPACE_CHUNKS {
        return Err(ProtocolError::InvalidWorkspacePayload);
    }
    if chunks.iter().any(|(_, c, _)| *c != count) {
        return Err(ProtocolError::InvalidWorkspacePayload);
    }
    if chunks.len() < usize::from(count) {
        return Ok(None);
    }
    let mut ordered: Vec<Option<&[u8]>> = vec![None; usize::from(count)];
    for (index, _, data) in chunks {
        let slot = usize::from(*index);
        if slot >= ordered.len() || data.len() > WORKSPACE_CHUNK_DATA {
            return Err(ProtocolError::InvalidWorkspacePayload);
        }
        if ordered[slot].is_some() {
            continue; // duplicate retry
        }
        ordered[slot] = Some(data.as_slice());
    }
    if ordered.iter().any(|s| s.is_none()) {
        return Ok(None);
    }
    let mut payload = Vec::new();
    for piece in ordered.into_iter().flatten() {
        payload.extend_from_slice(piece);
    }
    if payload.len() > MAX_WORKSPACE_PAYLOAD {
        return Err(ProtocolError::WorkspacePayloadTooLarge(payload.len()));
    }
    Ok(Some(payload))
}

/// Encode [`ProtoEvent::HandoverEnterSpatial`].
pub fn encode_handover_enter_spatial(event: &ProtoEvent) -> Result<Vec<u8>, ProtocolError> {
    let (serial, workspace_rev, topology_gen, dest_display_id, local_x, local_y) = match event {
        ProtoEvent::HandoverEnterSpatial {
            serial,
            workspace_rev,
            topology_gen,
            dest_display_id,
            local_x,
            local_y,
        } => (
            *serial,
            *workspace_rev,
            *topology_gen,
            dest_display_id,
            *local_x,
            *local_y,
        ),
        _ => panic!("encode_handover_enter_spatial on wrong variant"),
    };
    if serial == 0 {
        return Err(ProtocolError::InvalidHandoverSerial);
    }
    let id = dest_display_id.as_bytes();
    if id.is_empty() || id.len() > MAX_DISPLAY_ID_LEN {
        return Err(ProtocolError::InvalidSpatialDisplayId);
    }
    let mut buf = Vec::with_capacity(1 + 4 + 8 + 4 + 1 + id.len() + 8);
    buf.push(EventType::HandoverEnterSpatial as u8);
    buf.extend_from_slice(&serial.to_be_bytes());
    buf.extend_from_slice(&workspace_rev.to_be_bytes());
    buf.extend_from_slice(&topology_gen.to_be_bytes());
    buf.push(id.len() as u8);
    buf.extend_from_slice(id);
    buf.extend_from_slice(&local_x.to_be_bytes());
    buf.extend_from_slice(&local_y.to_be_bytes());
    Ok(buf)
}

/// Decode a spatial handover enter frame.
pub fn decode_handover_enter_spatial(buf: &[u8]) -> Result<ProtoEvent, ProtocolError> {
    if buf.len() < 1 + 4 + 8 + 4 + 1 + 1 + 8 {
        return Err(ProtocolError::BufferTooSmall);
    }
    if !matches!(
        EventType::try_from(buf[0])?,
        EventType::HandoverEnterSpatial
    ) {
        return Err(ProtocolError::BufferTooSmall);
    }
    let serial = u32::from_be_bytes(buf[1..5].try_into().expect("4"));
    if serial == 0 {
        return Err(ProtocolError::InvalidHandoverSerial);
    }
    let workspace_rev = u64::from_be_bytes(buf[5..13].try_into().expect("8"));
    let topology_gen = u32::from_be_bytes(buf[13..17].try_into().expect("4"));
    let id_len = usize::from(buf[17]);
    if id_len == 0 || id_len > MAX_DISPLAY_ID_LEN {
        return Err(ProtocolError::InvalidSpatialDisplayId);
    }
    let expected = 18 + id_len + 8;
    if buf.len() != expected {
        return Err(ProtocolError::InvalidEventLength {
            event_type: EventType::HandoverEnterSpatial,
            actual: buf.len(),
            expected: "canonical spatial enter frame",
        });
    }
    let dest_display_id = std::str::from_utf8(&buf[18..18 + id_len])
        .map_err(|_| ProtocolError::InvalidSpatialDisplayId)?
        .to_string();
    let local_x = i32::from_be_bytes(buf[18 + id_len..22 + id_len].try_into().expect("4"));
    let local_y = i32::from_be_bytes(buf[22 + id_len..26 + id_len].try_into().expect("4"));
    Ok(ProtoEvent::HandoverEnterSpatial {
        serial,
        workspace_rev,
        topology_gen,
        dest_display_id,
        local_x,
        local_y,
    })
}

fn write_str(buf: &mut Vec<u8>, s: &str) -> Result<(), ProtocolError> {
    let bytes = s.as_bytes();
    if bytes.len() > 255 {
        return Err(ProtocolError::InvalidWorkspacePayload);
    }
    buf.push(bytes.len() as u8);
    buf.extend_from_slice(bytes);
    Ok(())
}

fn read_str(data: &mut &[u8]) -> Result<String, ProtocolError> {
    if data.is_empty() {
        return Err(ProtocolError::BufferTooSmall);
    }
    let len = usize::from(data[0]);
    *data = &data[1..];
    if data.len() < len {
        return Err(ProtocolError::BufferTooSmall);
    }
    let s =
        std::str::from_utf8(&data[..len]).map_err(|_| ProtocolError::InvalidWorkspacePayload)?;
    *data = &data[len..];
    Ok(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use input_event::workspace::DevicePlacement;

    fn sample_config() -> WorkspaceConfig {
        WorkspaceConfig {
            workspace_id: "ws-1".into(),
            revision: 7,
            authority_fingerprint: "auth-fp".into(),
            placements: vec![
                DevicePlacement {
                    peer_fingerprint: "peer-a".into(),
                    origin_in_workspace: (0, 0),
                },
                DevicePlacement {
                    peer_fingerprint: "peer-b".into(),
                    origin_in_workspace: (1920, 100),
                },
            ],
        }
    }

    #[test]
    fn workspace_payload_round_trip() {
        let config = sample_config();
        let bytes = encode_workspace_payload(&config).unwrap();
        let decoded = decode_workspace_payload(&bytes).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn workspace_chunks_reassemble() {
        let payload = encode_workspace_payload(&sample_config()).unwrap();
        let events = workspace_chunks(7, &payload).unwrap();
        assert!(!events.is_empty());
        let mut collected = Vec::new();
        for event in &events {
            let encoded = encode_workspace_chunk(event).unwrap();
            let decoded = decode_workspace_chunk(&encoded).unwrap();
            match decoded {
                ProtoEvent::WorkspaceChunk {
                    revision,
                    index,
                    count,
                    data,
                } => {
                    assert_eq!(revision, 7);
                    collected.push((index, count, data));
                }
                other => panic!("unexpected {other}"),
            }
        }
        let reassembled = try_reassemble_workspace(&collected).unwrap().unwrap();
        assert_eq!(reassembled, payload);
    }

    #[test]
    fn identified_topology_round_trip() {
        let topology = LocalTopology::try_new(
            vec![LocalDisplay {
                id: DisplayId::new("monitor-a").unwrap(),
                rect: DisplayRect::new(-100, 0, 1920, 1080).unwrap(),
                scale: Some(2.0),
                rotation: None,
            }],
            3,
            9,
        )
        .unwrap();
        let event = ProtoEvent::IdentifiedTopology {
            epoch: 3,
            generation: 9,
            topology: topology.clone(),
        };
        let bytes = encode_identified_topology(&event).unwrap();
        match decode_identified_topology(&bytes).unwrap() {
            ProtoEvent::IdentifiedTopology {
                epoch,
                generation,
                topology: decoded,
            } => {
                assert_eq!(epoch, 3);
                assert_eq!(generation, 9);
                assert_eq!(decoded.displays.len(), 1);
                assert_eq!(decoded.displays[0].id.as_str(), "monitor-a");
                assert_eq!(decoded.displays[0].rect, topology.displays[0].rect);
                assert!((decoded.displays[0].scale.unwrap() - 2.0).abs() < 0.001);
            }
            other => panic!("unexpected {other}"),
        }
    }

    #[test]
    fn spatial_enter_round_trip() {
        let event = ProtoEvent::HandoverEnterSpatial {
            serial: 42,
            workspace_rev: 5,
            topology_gen: 3,
            dest_display_id: "disp-1".into(),
            local_x: 10,
            local_y: 20,
        };
        let bytes = encode_handover_enter_spatial(&event).unwrap();
        match decode_handover_enter_spatial(&bytes).unwrap() {
            ProtoEvent::HandoverEnterSpatial {
                serial,
                workspace_rev,
                topology_gen,
                dest_display_id,
                local_x,
                local_y,
            } => {
                assert_eq!(serial, 42);
                assert_eq!(workspace_rev, 5);
                assert_eq!(topology_gen, 3);
                assert_eq!(dest_display_id, "disp-1");
                assert_eq!((local_x, local_y), (10, 20));
            }
            other => panic!("unexpected {other}"),
        }
    }

    #[test]
    fn unauthorized_empty_authority_rejected() {
        let mut config = sample_config();
        config.authority_fingerprint.clear();
        assert!(encode_workspace_payload(&config).is_err());
    }
}
