//! Bounded message reassembly only for directions handled by frameScript.
//! Other directions keep their original frame boundaries for plugin hooks.

use super::{
    Frame, MAX_FRAME_BYTES, OPCODE_BINARY, OPCODE_CLOSE, OPCODE_CONTINUATION, OPCODE_TEXT,
};
use std::io;

pub(super) struct Messages {
    enabled: bool,
    pending: Option<Frame>,
}

impl Messages {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            pending: None,
        }
    }

    pub(super) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Controls pass without waiting for the final fragment. No data from an
    /// incomplete message leaves this buffer, so a failed message cannot leak
    /// an unmodified prefix around the script.
    pub(super) fn push(&mut self, frame: Frame) -> io::Result<Option<Frame>> {
        if !self.enabled {
            return Ok(Some(frame));
        }
        match frame.opcode {
            OPCODE_TEXT | OPCODE_BINARY => {
                if self.pending.is_some() {
                    return Err(invalid("new WebSocket message before final continuation"));
                }
                if frame.fin {
                    Ok(Some(frame))
                } else {
                    self.pending = Some(frame);
                    Ok(None)
                }
            }
            OPCODE_CONTINUATION => {
                let Some(pending) = self.pending.as_mut() else {
                    return Err(invalid("WebSocket continuation without a message"));
                };
                // Each frame is already bounded by the decoder. Bound the sum
                // too: an unlimited sequence of small fragments is not bounded.
                if frame.payload.len() > MAX_FRAME_BYTES - pending.payload.len() {
                    return Err(invalid("WebSocket script message exceeds 8 MiB"));
                }
                pending.payload.extend_from_slice(&frame.payload);
                if frame.fin {
                    pending.fin = true;
                    Ok(self.pending.take())
                } else {
                    Ok(None)
                }
            }
            OPCODE_CLOSE => {
                self.pending = None;
                Ok(Some(frame))
            }
            _ => Ok(Some(frame)),
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::ws::{OPCODE_PING, OPCODE_PONG};

    fn frame(fin: bool, opcode: u8, payload: &[u8]) -> Frame {
        Frame {
            fin,
            opcode,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn preserves_binary_utf8_and_empty_messages_at_message_boundaries() {
        for (opcode, bytes) in [
            (OPCODE_TEXT, "中".as_bytes()),
            (OPCODE_BINARY, &[0, 255, 128][..]),
            (OPCODE_TEXT, &[][..]),
        ] {
            let mut messages = Messages::new(true);
            let split = bytes.len().min(1);
            assert!(
                messages
                    .push(frame(false, opcode, &bytes[..split]))
                    .unwrap()
                    .is_none()
            );
            assert!(
                messages
                    .push(frame(false, OPCODE_CONTINUATION, &[]))
                    .unwrap()
                    .is_none()
            );
            let got = messages
                .push(frame(true, OPCODE_CONTINUATION, &bytes[split..]))
                .unwrap()
                .unwrap();
            assert!(got.fin);
            assert_eq!(got.opcode, opcode);
            assert_eq!(got.payload, bytes);
            assert!(messages.pending.is_none());
        }
    }

    #[test]
    fn ping_and_pong_pass_and_close_discards_the_incomplete_message() {
        let mut messages = Messages::new(true);
        messages
            .push(frame(false, OPCODE_TEXT, b"partial"))
            .unwrap();
        for opcode in [OPCODE_PING, OPCODE_PONG] {
            assert_eq!(
                messages
                    .push(frame(true, opcode, b"probe"))
                    .unwrap()
                    .unwrap()
                    .opcode,
                opcode
            );
            assert!(messages.pending.is_some());
        }
        assert_eq!(
            messages
                .push(frame(true, OPCODE_CLOSE, &[]))
                .unwrap()
                .unwrap()
                .opcode,
            OPCODE_CLOSE
        );
        assert!(messages.pending.is_none());
    }

    #[test]
    fn aggregate_limit_accepts_exactly_eight_mib_but_not_one_byte_more() {
        let mut messages = Messages::new(true);
        messages
            .push(frame(false, OPCODE_BINARY, &vec![7; MAX_FRAME_BYTES - 1]))
            .unwrap();
        let got = messages
            .push(frame(true, OPCODE_CONTINUATION, &[8]))
            .unwrap()
            .unwrap();
        assert_eq!(got.payload.len(), MAX_FRAME_BYTES);
        assert_eq!(got.payload[MAX_FRAME_BYTES - 1], 8);
        messages
            .push(frame(false, OPCODE_BINARY, &vec![7; MAX_FRAME_BYTES]))
            .unwrap();
        assert_eq!(
            messages
                .push(frame(true, OPCODE_CONTINUATION, &[8]))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            messages.pending.as_ref().unwrap().payload.len(),
            MAX_FRAME_BYTES
        );
    }

    #[test]
    fn rejects_orphan_continuations_and_new_messages_mid_fragment() {
        let mut messages = Messages::new(true);
        assert!(
            messages
                .push(frame(true, OPCODE_CONTINUATION, b"orphan"))
                .is_err()
        );
        messages.push(frame(false, OPCODE_TEXT, b"start")).unwrap();
        assert!(messages.push(frame(true, OPCODE_BINARY, b"new")).is_err());
    }

    #[test]
    fn unhandled_directions_keep_the_original_fragment_boundaries() {
        let mut messages = Messages::new(false);
        for (fin, opcode) in [
            (false, OPCODE_TEXT),
            (false, OPCODE_CONTINUATION),
            (true, OPCODE_CONTINUATION),
        ] {
            let got = messages.push(frame(fin, opcode, b"a")).unwrap().unwrap();
            assert_eq!((got.fin, got.opcode), (fin, opcode));
        }
        assert!(messages.pending.is_none());
    }
}
