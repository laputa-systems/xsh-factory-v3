//! One-time FD 0 admission gate and sealed assignment packet verification.

use factory_protocol::{
    AssignmentPacketWireV2, FrameError, RESPONSE_FRAME_MAX_BYTES, SessionAdmissionFrameV2,
    verify_assignment_packet_v2,
};
use miniserde::json;
use pi_agent_protocol::JsonValue;
use std::fmt;
use std::io::{self, Read};

/// Maximum packet payload admitted after base64 decoding.
///
/// The bound is kept below the response-frame limit so the startup line and the later framed
/// transport cannot cause an unbounded allocation.  V2 may replace this constant as part of an
/// explicit protocol revision.
pub const DEFAULT_MAX_PACKET_BYTES: usize = 3 * 1024 * 1024 - 64 * 1024;

/// Maximum admission-line bytes, excluding its required trailing newline.
pub const MAX_ADMISSION_FRAME_BYTES: usize = RESPONSE_FRAME_MAX_BYTES - 1;

/// Read and verify configuration for the inherited descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionConfig {
    /// Maximum decoded assignment-packet bytes.
    pub max_packet_bytes: usize,
    /// Maximum admission JSON bytes before the newline.
    pub max_frame_bytes: usize,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        Self {
            max_packet_bytes: DEFAULT_MAX_PACKET_BYTES,
            max_frame_bytes: MAX_ADMISSION_FRAME_BYTES,
        }
    }
}

/// A fully verified immutable startup admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Admission {
    /// The daemon's startup attestation.
    pub frame: SessionAdmissionFrameV2,
    /// Canonical packet bytes carried by the attestation.
    pub packet_bytes: Vec<u8>,
    /// Parsed and digest-verified packet.
    pub packet: AssignmentPacketWireV2,
}

/// Failure before an agent may be constructed.
#[derive(Debug)]
pub enum AdmissionError {
    /// The inherited descriptor could not be read.
    Io(io::Error),
    /// The descriptor ended before a complete newline-delimited line.
    EndOfFile,
    /// The admission line exceeded its configured bound.
    AdmissionFrameOversized {
        /// Maximum permitted admission-line bytes.
        limit: usize,
    },
    /// The packet exceeded its configured decoded-byte bound.
    PacketOversized {
        /// Decoded packet bytes received.
        actual: usize,
        /// Maximum permitted decoded packet bytes.
        limit: usize,
    },
    /// The startup line was not UTF-8.
    InvalidUtf8,
    /// The startup line was not the canonical closed admission object.
    InvalidJson(String),
    /// The typed admission object rejected a field or identity.
    InvalidAdmission(String),
    /// The packet's base64 spelling was invalid or non-canonical.
    InvalidBase64,
    /// The packet failed canonical parsing or digest verification.
    InvalidPacket(FrameError),
    /// The out-of-band assignment identity did not match the packet identity.
    IdentityMismatch,
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "admission I/O failed: {error}"),
            Self::EndOfFile => f.write_str("daemon closed before session.admitted"),
            Self::AdmissionFrameOversized { limit } => {
                write!(f, "session.admitted frame exceeds {limit} bytes")
            }
            Self::PacketOversized { actual, limit } => {
                write!(f, "assignment packet is {actual} bytes, exceeding {limit}")
            }
            Self::InvalidUtf8 => f.write_str("session.admitted is not UTF-8"),
            Self::InvalidJson(detail) => write!(f, "invalid session.admitted JSON: {detail}"),
            Self::InvalidAdmission(detail) => {
                write!(f, "invalid session.admitted fields: {detail}")
            }
            Self::InvalidBase64 => f.write_str("packet_b64 is not canonical base64"),
            Self::InvalidPacket(error) => write!(f, "invalid assignment packet: {error}"),
            Self::IdentityMismatch => {
                f.write_str("admission and assignment packet identities differ")
            }
        }
    }
}

impl std::error::Error for AdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidPacket(error) => Some(error),
            _ => None,
        }
    }
}

/// Read and verify one admission from any blocking reader.
pub fn read_admission<R: Read>(
    reader: &mut R,
    config: AdmissionConfig,
) -> Result<Admission, AdmissionError> {
    if config.max_frame_bytes == 0 || config.max_packet_bytes == 0 {
        return Err(AdmissionError::InvalidAdmission(
            "admission bounds must be non-zero".to_owned(),
        ));
    }

    let mut line = Vec::with_capacity(config.max_frame_bytes.min(4096));
    let mut byte = [0_u8; 1];
    loop {
        let count = reader.read(&mut byte).map_err(AdmissionError::Io)?;
        if count == 0 {
            if line.len() >= config.max_frame_bytes {
                return Err(AdmissionError::AdmissionFrameOversized {
                    limit: config.max_frame_bytes,
                });
            }
            return Err(AdmissionError::EndOfFile);
        }
        if byte[0] == b'\n' {
            break;
        }
        if line.len() == config.max_frame_bytes {
            return Err(AdmissionError::AdmissionFrameOversized {
                limit: config.max_frame_bytes,
            });
        }
        line.push(byte[0]);
    }

    let text = std::str::from_utf8(&line).map_err(|_| AdmissionError::InvalidUtf8)?;
    let frame: SessionAdmissionFrameV2 =
        json::from_str(text).map_err(|error| AdmissionError::InvalidJson(format!("{error:?}")))?;
    // Canonicalize through the core protocol's BTreeMap-backed JSON value so the startup line
    // matches the daemon's sorted-key spelling (the typed struct declaration order is not the
    // wire order).  Compare the closed field set separately because Miniserde otherwise ignores
    // unknown DTO fields.
    let canonical = JsonValue::parse(text)
        .and_then(|value| value.to_json_string())
        .map_err(|error| AdmissionError::InvalidJson(error.to_string()))?;
    let expected_keys = [
        "assignment_id",
        "packet_b64",
        "packet_digest",
        "protocol_version",
        "session_id",
        "session_revision",
        "type",
    ];
    let Some(object) = JsonValue::parse(text)
        .ok()
        .and_then(|value| value.as_object().cloned())
    else {
        return Err(AdmissionError::InvalidJson(
            "admission is not a JSON object".to_owned(),
        ));
    };
    if canonical != text || object.keys().map(String::as_str).collect::<Vec<_>>() != expected_keys {
        return Err(AdmissionError::InvalidJson(
            "admission is not canonical closed JSON".to_owned(),
        ));
    }
    frame
        .validate()
        .map_err(|error| AdmissionError::InvalidAdmission(error.to_string()))?;

    let packet_bytes = decode_base64(&frame.packet_b64).ok_or(AdmissionError::InvalidBase64)?;
    if packet_bytes.len() > config.max_packet_bytes {
        return Err(AdmissionError::PacketOversized {
            actual: packet_bytes.len(),
            limit: config.max_packet_bytes,
        });
    }
    let packet = verify_assignment_packet_v2(&packet_bytes, &frame.packet_digest)
        .map_err(AdmissionError::InvalidPacket)?;
    if frame.assignment_id != packet.assignment_id.to_string() {
        return Err(AdmissionError::IdentityMismatch);
    }
    Ok(Admission {
        frame,
        packet_bytes,
        packet,
    })
}

/// Read one verified admission from the process's inherited full-duplex FD 0.
///
/// Opening `/dev/fd/0` is intentionally the only descriptor discovery performed by this helper;
/// no socket path, database URL, packet file, or resume identifier is accepted.
#[cfg(unix)]
pub fn read_admission_from_fd0(config: AdmissionConfig) -> Result<Admission, AdmissionError> {
    let mut input = std::fs::File::open("/dev/fd/0").map_err(AdmissionError::Io)?;
    read_admission(&mut input, config)
}

#[cfg(not(unix))]
pub fn read_admission_from_fd0(_config: AdmissionConfig) -> Result<Admission, AdmissionError> {
    Err(AdmissionError::InvalidAdmission(
        "inherited FD 0 admission is supported only on Unix".to_owned(),
    ))
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || !value.len().is_multiple_of(4) {
        return None;
    }
    let bytes = value.as_bytes();
    let padding = bytes.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2 || bytes[..bytes.len().saturating_sub(padding)].contains(&b'=') {
        return None;
    }
    let mut output = Vec::with_capacity((bytes.len() / 4) * 3 - padding);
    let (chunks, remainder) = bytes.as_chunks::<4>();
    debug_assert!(remainder.is_empty(), "base64 length was validated");
    for (index, chunk) in chunks.iter().enumerate() {
        let last = index + 1 == bytes.len() / 4;
        let a = base64_value(chunk[0])?;
        let b = base64_value(chunk[1])?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            base64_value(chunk[2])?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            base64_value(chunk[3])?
        };
        if (!last && (chunk[2] == b'=' || chunk[3] == b'='))
            || (chunk[2] == b'=' && chunk[3] != b'=')
        {
            return None;
        }
        if chunk[2] == b'=' && b & 0x0f != 0 || chunk[3] == b'=' && c & 0x03 != 0 {
            return None;
        }
        output.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            output.push((b << 4) | (c >> 2));
        }
        if chunk[3] != b'=' {
            output.push((c << 6) | d);
        }
    }
    Some(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use factory_protocol::PROTOCOL_VERSION_V2;
    use std::io::Cursor;

    fn admission_json(packet_b64: &str, assignment_id: &str) -> String {
        let frame = SessionAdmissionFrameV2 {
            r#type: "session.admitted".to_owned(),
            protocol_version: PROTOCOL_VERSION_V2,
            assignment_id: assignment_id.to_owned(),
            session_id: 7,
            session_revision: 3,
            packet_digest: "0".repeat(64),
            packet_b64: packet_b64.to_owned(),
        };
        json::to_string(&frame)
    }

    #[test]
    fn admission_reader_rejects_missing_newline_without_unbounded_read() {
        let mut input = Cursor::new(b"{}".to_vec());
        let error = read_admission(
            &mut input,
            AdmissionConfig {
                max_packet_bytes: 8,
                max_frame_bytes: 2,
            },
        )
        .expect_err("unterminated admission must fail");
        assert!(matches!(
            error,
            AdmissionError::AdmissionFrameOversized { limit: 2 }
        ));
    }

    #[test]
    fn base64_decoder_rejects_non_canonical_trailing_bits() {
        assert_eq!(decode_base64("YQ=="), Some(b"a".to_vec()));
        assert!(decode_base64("YR==").is_none());
        assert!(decode_base64("YWJ=").is_none());
        assert!(decode_base64("Y=Jj").is_none());
    }

    #[test]
    fn base64_decoder_accepts_padding_only_on_the_final_chunk() {
        assert_eq!(decode_base64("YWJjZGU="), Some(b"abcde".to_vec()));
    }

    #[test]
    fn admission_reader_rejects_unknown_fields_before_packet_work() {
        let mut input = Cursor::new(
            (admission_json("e30=", "1") + "\n")
                .replace("}", ",\"unexpected\":true}")
                .into_bytes(),
        );
        let error = read_admission(&mut input, AdmissionConfig::default())
            .expect_err("unknown startup fields must fail closed");
        assert!(matches!(error, AdmissionError::InvalidJson(_)));
    }
}
