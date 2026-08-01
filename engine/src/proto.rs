//! docs/PROTOCOL.md v0 のフレーミング。
//! `[u32 LE: payload 長][u8: type][payload]`

use std::io::{self, Read, Write};

pub const VERSION: u32 = 0;

// クライアント → エンジン
pub const HELLO: u8 = 0x01;
pub const START: u8 = 0x02;
pub const AUDIO: u8 = 0x03;
pub const STOP: u8 = 0x04;

// エンジン → クライアント
pub const READY: u8 = 0x81;
pub const PARTIAL: u8 = 0x82;
pub const COMMIT: u8 = 0x83;
pub const STOPPED: u8 = 0x84;
pub const ERROR: u8 = 0xFF;

/// 異常なフレームで OOM しないための上限。AUDIO チャンクは高々数十 KB
pub const MAX_PAYLOAD: u32 = 16 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub payload: Vec<u8>,
}

pub fn write_frame(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    todo!()
}

pub fn read_frame(r: &mut impl Read) -> io::Result<Frame> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, COMMIT, br#"{"text":"a"}"#).unwrap();
        let frame = read_frame(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(frame.kind, COMMIT);
        assert_eq!(frame.payload, br#"{"text":"a"}"#);
    }

    #[test]
    fn roundtrip_empty_payload() {
        let mut buf = Vec::new();
        write_frame(&mut buf, STOP, b"").unwrap();
        let frame = read_frame(&mut Cursor::new(&buf)).unwrap();
        assert_eq!(
            frame,
            Frame {
                kind: STOP,
                payload: vec![]
            }
        );
    }

    #[test]
    fn sequential_frames() {
        let mut buf = Vec::new();
        write_frame(&mut buf, HELLO, b"one").unwrap();
        write_frame(&mut buf, AUDIO, &[1, 2, 3, 4]).unwrap();
        let mut cursor = Cursor::new(&buf);
        assert_eq!(read_frame(&mut cursor).unwrap().payload, b"one");
        assert_eq!(read_frame(&mut cursor).unwrap().payload, [1, 2, 3, 4]);
    }

    #[test]
    fn wire_format_is_len_le_then_type() {
        let mut buf = Vec::new();
        write_frame(&mut buf, AUDIO, &[0xAA, 0xBB]).unwrap();
        assert_eq!(buf, [2, 0, 0, 0, AUDIO, 0xAA, 0xBB]);
    }

    #[test]
    fn eof_is_error() {
        assert!(read_frame(&mut Cursor::new(&[] as &[u8])).is_err());
        // ヘッダ途中で切れる
        assert!(read_frame(&mut Cursor::new(&[2u8, 0, 0])).is_err());
        // payload が長さに満たない
        assert!(read_frame(&mut Cursor::new(&[5u8, 0, 0, 0, AUDIO, 1])).is_err());
    }

    #[test]
    fn oversized_payload_is_rejected_without_allocation() {
        let mut header = vec![0xFF, 0xFF, 0xFF, 0xFF, AUDIO];
        header.extend([0u8; 8]);
        assert!(read_frame(&mut Cursor::new(&header)).is_err());
    }
}
