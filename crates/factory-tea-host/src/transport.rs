//! Bounded, sequential length-prefixed transport over the inherited descriptor.

use factory_protocol::{FRAME_PREFIX_BYTES, FrameError, decode_frame, encode_frame};
use std::fmt;
use std::io::{self, Read, Write};

/// Maximum request payload accepted by the daemon actor route.
pub const MAX_REQUEST_FRAME_BYTES: usize = factory_protocol::REQUEST_FRAME_MAX_BYTES;
/// Maximum response payload accepted by the daemon actor route.
pub const MAX_RESPONSE_FRAME_BYTES: usize = factory_protocol::RESPONSE_FRAME_MAX_BYTES;

/// Errors from bounded frame reads/writes.
#[derive(Debug)]
pub enum FrameTransportError {
    /// The peer stream failed.
    Io(io::Error),
    /// A complete frame violated the Factory frame contract.
    Frame(FrameError),
    /// The peer response did not match the request identity.
    ResponseIdentity {
        /// Operation the caller expected.
        expected_operation: String,
        /// Request ID the caller expected.
        expected_request_id: String,
    },
    /// The response had no operation/request identity fields.
    InvalidResponseIdentity,
}

impl fmt::Display for FrameTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "actor transport I/O failed: {error}"),
            Self::Frame(error) => write!(f, "actor frame rejected: {error}"),
            Self::ResponseIdentity {
                expected_operation,
                expected_request_id,
            } => write!(
                f,
                "actor response identity did not match operation {expected_operation:?}, request {expected_request_id:?}"
            ),
            Self::InvalidResponseIdentity => f.write_str("actor response identity is invalid"),
        }
    }
}

impl std::error::Error for FrameTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Frame(error) => Some(error),
            _ => None,
        }
    }
}

/// Read exactly one bounded frame from a blocking stream.
pub fn read_frame<R: Read>(reader: &mut R, maximum: usize) -> Result<Vec<u8>, FrameTransportError> {
    let mut prefix = [0_u8; FRAME_PREFIX_BYTES];
    read_exact_or_eof(reader, &mut prefix)?;
    let payload_length = u32::from_be_bytes(prefix) as usize;
    if payload_length > maximum {
        return Err(FrameTransportError::Frame(FrameError::Oversized {
            actual: payload_length,
            maximum,
        }));
    }
    let mut frame = Vec::with_capacity(FRAME_PREFIX_BYTES + payload_length);
    frame.extend_from_slice(&prefix);
    frame.resize(FRAME_PREFIX_BYTES + payload_length, 0);
    reader
        .read_exact(&mut frame[FRAME_PREFIX_BYTES..])
        .map_err(FrameTransportError::Io)?;
    decode_frame(&frame, maximum)
        .map(<[u8]>::to_vec)
        .map_err(FrameTransportError::Frame)
}

/// Write one complete length-prefixed frame, handling short writes.
pub fn write_frame<W: Write>(
    writer: &mut W,
    payload: &[u8],
    maximum: usize,
) -> Result<(), FrameTransportError> {
    let frame = encode_frame(payload, maximum).map_err(FrameTransportError::Frame)?;
    let mut offset = 0;
    while offset < frame.len() {
        let written = writer
            .write(&frame[offset..])
            .map_err(FrameTransportError::Io)?;
        if written == 0 {
            return Err(FrameTransportError::Io(io::Error::new(
                io::ErrorKind::WriteZero,
                "actor transport write made no progress",
            )));
        }
        offset += written;
    }
    writer.flush().map_err(FrameTransportError::Io)
}

/// Sequential request/response client over separate read/write handles.
///
/// The daemon route allows exactly one request in flight. This type intentionally does not
/// spawn a reader or background task, so EOF and cancellation remain owned by the host process.
pub struct FrameClient<R, W> {
    reader: R,
    writer: W,
    next_request_id: u64,
}

impl<R, W> FrameClient<R, W>
where
    R: Read,
    W: Write,
{
    /// Construct a client around the already-connected actor descriptor.
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_request_id: 0,
        }
    }

    /// Exchange one pre-encoded request frame and return its response payload.
    pub fn exchange(&mut self, request_frame: &[u8]) -> Result<Vec<u8>, FrameTransportError> {
        // Validate before writing so malformed host data cannot reach the daemon.
        let request_payload = decode_frame(request_frame, MAX_REQUEST_FRAME_BYTES)
            .map_err(FrameTransportError::Frame)?;
        write_frame(&mut self.writer, request_payload, MAX_REQUEST_FRAME_BYTES)?;
        read_frame(&mut self.reader, MAX_RESPONSE_FRAME_BYTES)
    }

    /// Allocate a process-local request ID for callers building typed RPC payloads.
    pub fn next_request_id(&mut self) -> String {
        self.next_request_id = self.next_request_id.saturating_add(1);
        format!("factory-tea-host-request-{}", self.next_request_id)
    }

    /// Validate a response envelope after a typed operation has been decoded by the caller.
    pub fn validate_response_identity(
        response_operation: Option<&str>,
        response_request_id: Option<&str>,
        expected_operation: &str,
        expected_request_id: &str,
    ) -> Result<(), FrameTransportError> {
        if response_operation.is_none() || response_request_id.is_none() {
            return Err(FrameTransportError::InvalidResponseIdentity);
        }
        if response_operation != Some(expected_operation)
            || response_request_id != Some(expected_request_id)
        {
            return Err(FrameTransportError::ResponseIdentity {
                expected_operation: expected_operation.to_owned(),
                expected_request_id: expected_request_id.to_owned(),
            });
        }
        Ok(())
    }
}

fn read_exact_or_eof<R: Read>(
    reader: &mut R,
    buffer: &mut [u8],
) -> Result<(), FrameTransportError> {
    let mut offset = 0;
    while offset < buffer.len() {
        let count = reader
            .read(&mut buffer[offset..])
            .map_err(FrameTransportError::Io)?;
        if count == 0 {
            return Err(FrameTransportError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "actor peer closed before a complete frame",
            )));
        }
        offset += count;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_round_trip_handles_short_writes() {
        let payload = b"hello";
        let mut encoded = Vec::new();
        write_frame(&mut encoded, payload, 32).expect("frame writes");
        assert_eq!(
            read_frame(&mut Cursor::new(encoded), 32).expect("frame reads"),
            payload
        );
    }

    #[test]
    fn frame_read_rejects_oversized_length_before_allocation() {
        let encoded = (33_u32).to_be_bytes();
        let error = read_frame(&mut Cursor::new(encoded.to_vec()), 32)
            .expect_err("oversized frame must fail");
        assert!(matches!(
            error,
            FrameTransportError::Frame(FrameError::Oversized {
                actual: 33,
                maximum: 32
            })
        ));
    }

    #[test]
    fn response_identity_is_strict_and_request_ids_are_monotonic() {
        let mut client = FrameClient::new(Cursor::new(Vec::new()), Vec::new());
        let first = client.next_request_id();
        let second = client.next_request_id();
        assert_ne!(first, second);
        FrameClient::<Cursor<Vec<u8>>, Vec<u8>>::validate_response_identity(
            Some("session.verify_packet"),
            Some(&first),
            "session.verify_packet",
            &first,
        )
        .expect("matching identity");
        assert!(
            FrameClient::<Cursor<Vec<u8>>, Vec<u8>>::validate_response_identity(
                Some("session.seal_artifact"),
                Some(&first),
                "session.verify_packet",
                &first,
            )
            .is_err()
        );
    }
}
