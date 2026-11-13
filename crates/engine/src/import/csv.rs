use std::io::{BufRead, BufReader, Cursor, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::model::ImportError;

pub const MAX_RECORD: usize = 8 * 1024 * 1024;
pub const MAX_FIELDS: usize = 1024;
pub const VERSION: u8 = 1;

pub struct Record {
    pub fields: Vec<Option<String>>,
    pub line: u64,
    pub number: u64,
    pub bytes: u64,
}

#[derive(Clone, Copy)]
enum State {
    Start,
    Plain,
    Quoted,
    Closed,
}

pub struct Parser<R: Read> {
    reader: BufReader<std::io::Chain<Cursor<Vec<u8>>, R>>,
    bytes: u64,
    line: u64,
    record: u64,
}

impl<R: Read> Parser<R> {
    pub fn new(mut reader: R) -> Result<Self, ImportError> {
        let mut prefix = Vec::with_capacity(3);
        for _ in 0..3 {
            let mut byte = [0];
            if reader.read(&mut byte).map_err(io)? == 0 {
                break;
            }
            prefix.push(byte[0]);
        }
        let bom = prefix == [0xef, 0xbb, 0xbf];
        if bom {
            prefix.clear();
        }
        Ok(Self {
            reader: BufReader::with_capacity(64 * 1024, Cursor::new(prefix).chain(reader)),
            bytes: if bom { 3 } else { 0 },
            line: 1,
            record: 1,
        })
    }

    fn fail(&self, message: &str) -> ImportError {
        ImportError::Csv { line: self.line, record: self.record, message: message.into() }
    }

    fn field(&self, bytes: Vec<u8>, quoted: bool) -> Result<Option<String>, ImportError> {
        if bytes.is_empty() && !quoted {
            return Ok(None);
        }
        String::from_utf8(bytes).map(Some).map_err(|_| self.fail("Invalid UTF-8"))
    }

    pub fn next(&mut self, stop: &AtomicBool) -> Result<Option<Record>, ImportError> {
        let start = self.bytes;
        let line = self.line;
        let number = self.record;
        let mut fields = Vec::new();
        let mut field = Vec::new();
        let mut state = State::Start;
        let mut quoted = false;
        loop {
            if self.bytes.is_multiple_of(4096) && stop.load(Ordering::Relaxed) {
                return Err(ImportError::Cancelled);
            }
            let byte = self.reader.fill_buf().map_err(io)?.first().copied();
            let Some(byte) = byte else {
                if matches!(state, State::Quoted) {
                    return Err(self.fail("Unclosed quoted field"));
                }
                if self.bytes == start {
                    return Ok(None);
                }
                fields.push(self.field(field, quoted)?);
                self.record += 1;
                return Ok(Some(Record { fields, bytes: self.bytes, line, number }));
            };
            self.reader.consume(1);
            self.bytes += 1;
            if self.bytes - start > MAX_RECORD as u64 {
                return Err(self.fail("Record exceeds 8 MiB"));
            }
            if byte == 0 {
                return Err(self.fail("NUL is not valid PostgreSQL text"));
            }
            match (state, byte) {
                (State::Quoted, b'"') => state = State::Closed,
                (State::Quoted, _) => {
                    if byte == b'\n' {
                        self.line += 1;
                    }
                    field.push(byte);
                }
                (State::Closed, b'"') => {
                    field.push(byte);
                    state = State::Quoted;
                }
                (State::Start, b'"') => {
                    state = State::Quoted;
                    quoted = true;
                }
                (_, b',') => {
                    fields.push(self.field(std::mem::take(&mut field), quoted)?);
                    if fields.len() >= MAX_FIELDS {
                        return Err(self.fail("Record exceeds 1024 fields"));
                    }
                    state = State::Start;
                    quoted = false;
                }
                (_, b'\r' | b'\n') => {
                    if byte == b'\r' {
                        if self.reader.fill_buf().map_err(io)?.first() != Some(&b'\n') {
                            return Err(self.fail("Record CR must be followed by LF"));
                        }
                        self.reader.consume(1);
                        self.bytes += 1;
                        if self.bytes - start > MAX_RECORD as u64 {
                            return Err(self.fail("Record exceeds 8 MiB"));
                        }
                    }
                    fields.push(self.field(field, quoted)?);
                    self.line += 1;
                    self.record += 1;
                    return Ok(Some(Record { fields, bytes: self.bytes, line, number }));
                }
                (State::Closed, _) => return Err(self.fail("Unexpected text after closing quote")),
                (_, b'"') => return Err(self.fail("Quote inside an unquoted field")),
                (_, _) => {
                    field.push(byte);
                    state = State::Plain;
                }
            }
        }
    }
}

pub fn encode(fields: &[Option<String>], mapping: &[Option<String>], out: &mut Vec<u8>) {
    let mut first = true;
    for (value, target) in fields.iter().zip(mapping) {
        if target.is_none() {
            continue;
        }
        if !first {
            out.push(b',');
        }
        first = false;
        if let Some(value) = value {
            out.push(b'"');
            for byte in value.bytes() {
                out.push(byte);
                if byte == b'"' {
                    out.push(byte);
                }
            }
            out.push(b'"');
        }
    }
    out.push(b'\n');
}

pub fn io(error: std::io::Error) -> ImportError {
    ImportError::Io { message: error.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(bytes: &[u8]) -> Result<Vec<Vec<Option<String>>>, ImportError> {
        let mut parser = Parser::new(bytes)?;
        let mut rows = Vec::new();
        while let Some(row) = parser.next(&AtomicBool::new(false))? {
            rows.push(row.fields);
        }
        Ok(rows)
    }
    #[test]
    fn preserves_text_nulls_and_quote_provenance() {
        let rows = parse("\u{feff}a,b,c\r\n\"x,y\",\"a\"\"b\n雪\r\nz\",\r\n,\"\",9007199254740993".as_bytes()).unwrap();
        assert_eq!(rows[1], vec![Some("x,y".into()), Some("a\"b\n雪\r\nz".into()), None]);
        assert_eq!(rows[2], vec![None, Some("".into()), Some("9007199254740993".into())]);
        let mut encoded = Vec::new();
        encode(&rows[2], &[Some("a".into()), Some("b".into()), Some("c".into())], &mut encoded);
        assert_eq!(parse(&encoded).unwrap(), vec![rows[2].clone()]);
    }
    #[test]
    fn handles_one_byte_reads() {
        struct One<'a>(&'a [u8]);
        impl Read for One<'_> {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                self.0.read(&mut b[..1])
            }
        }
        let mut parser = Parser::new(One("\u{feff}\"a\"\"b\r\nc\",\"\"\r\nx,".as_bytes())).unwrap();
        assert_eq!(
            parser.next(&AtomicBool::new(false)).unwrap().unwrap().fields,
            vec![Some("a\"b\r\nc".into()), Some("".into())]
        );
        let last = parser.next(&AtomicBool::new(false)).unwrap().unwrap();
        assert_eq!(last.fields, vec![Some("x".into()), None]);
        assert_eq!(last.bytes, 19);
    }
    #[test]
    fn rejects_invalid_and_unbounded_input() {
        for bytes in [b"a\"b".as_slice(), b"\"open", b"\"a\"x", b"a\0b", b"\xff", b"a\rb"] {
            assert!(matches!(parse(bytes), Err(ImportError::Csv { .. })), "{bytes:?}");
        }
        assert!(parse(&vec![b','; MAX_FIELDS]).is_err());
        assert!(parse(&vec![b'x'; MAX_RECORD + 1]).is_err());
        assert_eq!(parse(b"").unwrap(), Vec::<Vec<Option<String>>>::new());
        assert_eq!(parse(b"\n").unwrap(), vec![vec![None]]);
    }
}
