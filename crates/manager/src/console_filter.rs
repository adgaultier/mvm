//! Strips terminal *query* sequences from the recorded console stream.
//!
//! `console.log` is a byte-faithful recording of a tty session, and
//! `mvm logs` / `mvm attach`'s backlog replay it verbatim. A query is a
//! sequence that asks the terminal to *answer* — DSR (`ESC[6n`, "where is
//! the cursor?") and Device Attributes (`ESC[c`). Replaying one is always
//! wrong: it was already answered, live, by whoever was attached at record
//! time, and a second answer is written into the reader's input buffer
//! instead — which surfaces as stray `^[[1;5R` in their shell.
//!
//! Only queries are dropped; colours, cursor motion and erases are real
//! output and stay.
//!
//! The broadcast channel carries the console byte-exact, because one of its
//! consumers must see queries: an interactive session (`mvm attach`,
//! `mvm run -it`) owns the terminal and reads the reply. Every *other*
//! consumer must not, so the logs route runs this same filter over the live
//! stream unless the client asked for `?raw=true` — filtering the recording
//! alone would leave `mvm logs -f`'s live tail unprotected.

/// Longest escape sequence held while waiting for its final byte. Anything
/// longer is not a query we recognise, so it is released rather than
/// buffered forever (a binary-spewing workload must not grow this).
const MAX_PENDING: usize = 64;

/// Incremental filter: feed it console chunks, get back the bytes to record.
/// A sequence split across two reads is held until it completes.
#[derive(Default)]
pub struct QueryFilter {
    pending: Vec<u8>,
}

impl QueryFilter {
    /// Filter one chunk. Bytes of an incomplete trailing sequence are kept
    /// for the next call rather than emitted.
    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            if self.pending.is_empty() {
                if b == 0x1b {
                    self.pending.push(b);
                } else {
                    out.push(b);
                }
                continue;
            }

            self.pending.push(b);

            // ESC + one byte: only CSI keeps going; ESC Z is an obsolete
            // Device Attributes query and is dropped whole.
            if self.pending.len() == 2 {
                match b {
                    b'[' => {}
                    b'Z' => self.pending.clear(),
                    _ => out.append(&mut self.pending),
                }
                continue;
            }

            // Inside a CSI: parameter bytes (0x30-0x3F) and intermediates
            // (0x20-0x2F) continue it; 0x40-0x7E terminates it.
            if (0x40..=0x7e).contains(&b) {
                // DSR ends in 'n', DA in 'c' — the reply-soliciting finals.
                if b == b'n' || b == b'c' {
                    self.pending.clear();
                } else {
                    out.append(&mut self.pending);
                }
            } else if !(0x20..=0x3f).contains(&b) || self.pending.len() > MAX_PENDING {
                // Malformed or absurdly long: not a query, pass it through.
                out.append(&mut self.pending);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(chunks: &[&[u8]]) -> Vec<u8> {
        let mut f = QueryFilter::default();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(f.filter(c));
        }
        out
    }

    #[test]
    fn drops_cursor_position_query() {
        // The exact recording an alpine prompt produces.
        assert_eq!(filtered(&[b"~ # \x1b[6n"]), b"~ # ");
    }

    #[test]
    fn drops_device_attribute_queries() {
        assert_eq!(filtered(&[b"a\x1b[cb"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b[>0cb"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b[?6nb"]), b"ab");
        assert_eq!(filtered(&[b"a\x1bZb"]), b"ab");
    }

    #[test]
    fn keeps_colours_and_cursor_motion() {
        // Real output: SGR, cursor home, erase-to-end all survive.
        let keep: &[u8] = b"\x1b[31mred\x1b[0m\x1b[H\x1b[2J\x1b[1;5H";
        assert_eq!(filtered(&[keep]), keep);
    }

    #[test]
    fn handles_a_sequence_split_across_chunks() {
        assert_eq!(filtered(&[b"~ # \x1b", b"[6n", b"done"]), b"~ # done");
        assert_eq!(filtered(&[b"\x1b[", b"31mred"]), b"\x1b[31mred");
    }

    #[test]
    fn passes_plain_and_binary_bytes_through() {
        assert_eq!(filtered(&[b"plain\r\ntext\n"]), b"plain\r\ntext\n");
        assert_eq!(
            filtered(&[&[0x00, 0xff, 0x1b, 0x00]]),
            &[0x00, 0xff, 0x1b, 0x00]
        );
    }

    #[test]
    fn does_not_buffer_forever_on_a_lone_escape() {
        let mut f = QueryFilter::default();
        // An ESC followed by a long run of parameter bytes is released
        // rather than held, so a stuck sequence cannot eat the stream.
        let mut long = b"\x1b[".to_vec();
        long.extend(std::iter::repeat_n(b'0', 200));
        let out = f.filter(&long);
        assert!(
            out.len() > 100,
            "expected the run to be released, got {}",
            out.len()
        );
    }
}
