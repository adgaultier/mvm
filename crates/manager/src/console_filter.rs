//! Strips terminal *query* and *mode-change* sequences from the recorded
//! console stream.
//!
//! `console.log` is a byte-faithful recording of a tty session, and
//! `mvm logs` / `mvm attach`'s backlog replay it verbatim. A query is a
//! sequence that asks the terminal to *answer* — DSR (`ESC[6n`, "where is
//! the cursor?"), Device Attributes (`ESC[c`), OSC colour/palette queries
//! (`ESC]10;?`, `ESC]4;N;?`, `ESC]1337;Capabilities=?`), XTGETTCAP and
//! DECRQSS (`DCS`), XTVERSION (`ESC[>q`) and DECRQM (`ESC[?mode$p`).
//! Replaying one is always wrong: it was already answered, live, by whoever
//! was attached at record time, and a second answer is written into the
//! reader's input buffer instead — which surfaces as stray `^[[1;5R` or
//! `;10;rgb:…` replies echoed by their shell once the reader exits.
//!
//! Mode changes are dropped for the same reason: a guest TUI's DECSETs
//! (mouse reporting, focus events, bracketed paste, alt screen), kitty
//! keyboard pushes and modifyOtherKeys sets live in the terminal *emulator*,
//! and replaying them rewires the *reader's* terminal with nobody left to
//! undo it — mouse moves or `ESC[97;5u`-style ctrl+letter keycodes then land
//! in their shell.
//!
//! Colours, cursor motion and erases are real output and stay. In
//! particular, an OSC that *sets* a value (`ESC]10;rgb:…`) is kept — only
//! payload-ending-in-`?` OSCs are queries.
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

/// Parser state: where in a possible escape sequence we are.
#[derive(Default, PartialEq)]
enum State {
    /// Not in a sequence.
    #[default]
    Ground,
    /// Just saw ESC.
    Esc,
    /// Inside a CSI (`ESC [`), ends on a byte in 0x40-0x7E.
    Csi,
    /// Inside an OSC (`ESC ]`), ends on BEL or ST.
    Osc,
    /// Inside an OSC and just saw ESC (ST is `ESC \`).
    OscEsc,
    /// Inside a DCS (`ESC P`), ends on ST.
    Dcs,
    /// Inside a DCS and just saw ESC.
    DcsEsc,
}

/// Incremental filter: feed it console chunks, get back the bytes to record.
/// A sequence split across two reads is held until it completes.
#[derive(Default)]
pub struct QueryFilter {
    pending: Vec<u8>,
    state: State,
}

impl QueryFilter {
    /// Filter one chunk. Bytes of an incomplete trailing sequence are kept
    /// for the next call rather than emitted.
    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.pending.push(b);
                        self.state = State::Esc;
                    } else {
                        out.push(b);
                    }
                }
                State::Esc => {
                    self.pending.push(b);
                    match b {
                        b'[' => self.state = State::Csi,
                        b']' => self.state = State::Osc,
                        b'P' => self.state = State::Dcs,
                        // ESC Z is an obsolete Device Attributes query.
                        b'Z' => {
                            self.pending.clear();
                            self.state = State::Ground;
                        }
                        _ => {
                            out.append(&mut self.pending);
                            self.state = State::Ground;
                        }
                    }
                }
                State::Csi => {
                    self.pending.push(b);
                    if (0x40..=0x7e).contains(&b) {
                        if is_dropped_csi(&self.pending) {
                            self.pending.clear();
                        } else {
                            out.append(&mut self.pending);
                        }
                        self.state = State::Ground;
                    } else if !(0x20..=0x3f).contains(&b) || self.pending.len() > MAX_PENDING {
                        // Malformed or absurdly long: not a query, pass it through.
                        out.append(&mut self.pending);
                        self.state = State::Ground;
                    }
                }
                State::Osc => {
                    if b == 0x07 {
                        // BEL terminates an OSC.
                        self.pending.push(b);
                        self.finish_sequence(&mut out, 1, is_dropped_osc);
                        self.state = State::Ground;
                    } else if b == 0x1b {
                        self.state = State::OscEsc;
                    } else {
                        self.pending.push(b);
                        if self.pending.len() > MAX_PENDING {
                            out.append(&mut self.pending);
                            self.state = State::Ground;
                        }
                    }
                }
                State::OscEsc => {
                    if b == b'\\' {
                        self.pending.push(0x1b);
                        self.pending.push(b);
                        self.finish_sequence(&mut out, 2, is_dropped_osc);
                        self.state = State::Ground;
                    } else {
                        // A stray ESC inside the payload: it is content, not ST.
                        self.pending.push(0x1b);
                        self.pending.push(b);
                        self.state = State::Osc;
                    }
                }
                State::Dcs => {
                    if b == 0x1b {
                        self.state = State::DcsEsc;
                    } else {
                        self.pending.push(b);
                        if self.pending.len() > MAX_PENDING {
                            out.append(&mut self.pending);
                            self.state = State::Ground;
                        }
                    }
                }
                State::DcsEsc => {
                    if b == b'\\' {
                        self.pending.push(0x1b);
                        self.pending.push(b);
                        self.finish_sequence(&mut out, 2, is_dropped_dcs);
                        self.state = State::Ground;
                    } else {
                        self.pending.push(0x1b);
                        self.pending.push(b);
                        self.state = State::Dcs;
                    }
                }
            }
        }
        out
    }

    /// End of an OSC/DCS sequence: `terminator_len` trailing bytes of
    /// `pending` are the terminator (BEL or ST); the payload sits between
    /// the `ESC ]`/`ESC P` opener and it. Drop the sequence if the payload
    /// is a query, keep it (terminator included) if it *sets* something.
    fn finish_sequence(
        &mut self,
        out: &mut Vec<u8>,
        terminator_len: usize,
        is_query: fn(&[u8]) -> bool,
    ) {
        let end = self.pending.len() - terminator_len;
        if is_query(&self.pending[2..end]) {
            self.pending.clear();
        } else {
            out.append(&mut self.pending);
        }
    }
}

/// Whether a complete CSI sequence is a terminal query to drop.
fn is_dropped_csi(pending: &[u8]) -> bool {
    let Some(&final_byte) = pending.last() else {
        return false;
    };
    let marker = pending.get(2).copied();
    match final_byte {
        // DSR ends in 'n', DA in 'c' — the reply-soliciting finals.
        b'n' | b'c' => true,
        // XTVERSION query: CSI > Pp q. (A plain `CSI SP q` is DECSCUSR, a
        // cursor-style *set*, and must stay.)
        b'q' => marker == Some(b'>'),
        // DECRQM query: CSI ? mode $ p. (`CSI ! p` is DECSCL, a set: keep.)
        b'p' => pending[2..].contains(&b'$'),
        // DECSET/DECRST of a private mode (`CSI ? … h`/`l`): mouse
        // reporting, focus events, bracketed paste, alt screen, application
        // cursor keys… Replaying one would toggle the *reader's* terminal
        // modes with nobody to restore them — the TUI that set them is long
        // gone. Not output: drop.
        b'h' | b'l' => marker == Some(b'?'),
        // Kitty keyboard protocol: query (`?u`), push (`>u`), set (`=u`),
        // pop (`<u`) — same leak class: replayed, they rewire the reader's
        // keyboard into CSI-u keycodes.
        b'u' => matches!(marker, Some(b'?') | Some(b'>') | Some(b'=') | Some(b'<')),
        // modifyOtherKeys (xterm): `CSI > 4 ; mode m`. Plain `CSI Ps m` is
        // SGR (colours) — real output, kept.
        b'm' => marker == Some(b'>'),
        _ => false,
    }
}

/// Whether a complete OSC payload is a terminal query to drop: queries end
/// in `?` (`10;?`, `4;0;?`, `1337;Capabilities=?`); anything else *sets* a
/// value (title, colours) and is real output.
fn is_dropped_osc(payload: &[u8]) -> bool {
    payload.ends_with(b"?")
}

/// Whether a complete DCS payload is a terminal query to drop: XTGETTCAP
/// (`+q…`) and DECRQSS (`$q…`).
fn is_dropped_dcs(payload: &[u8]) -> bool {
    payload.starts_with(b"+q") || payload.starts_with(b"$q")
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
    fn drops_xtversion_and_decrqm_queries() {
        assert_eq!(filtered(&[b"a\x1b[>qb"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b[>0qb"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b[?1004$pb"]), b"ab");
    }

    #[test]
    fn keeps_csi_that_are_not_queries() {
        // DECSCL soft reset and DECSCUSR cursor style are sets, not queries.
        let keep: &[u8] = b"\x1b[!p\x1b[2 q";
        assert_eq!(filtered(&[keep]), keep);
    }

    #[test]
    fn drops_terminal_mode_changes() {
        // DECSET/DECRST private modes: mouse, focus, paste, alt screen, app
        // cursor keys — replaying them toggles the reader's terminal.
        assert_eq!(filtered(&[b"a\x1b[?1003hb"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b[?1003lb"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b[?2004hb"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b[?1049hb"]), b"ab");
        // Kitty keyboard: query, push, set, pop.
        assert_eq!(filtered(&[b"a\x1b[?ub"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b[>1ub"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b[=1;1ub"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b[<ub"]), b"ab");
        // modifyOtherKeys (xterm).
        assert_eq!(filtered(&[b"a\x1b[>4;2mb"]), b"ab");
    }

    #[test]
    fn keeps_non_private_sets_and_sgr() {
        // Non-private CSI h/l (e.g. insert mode) and plain SGR stay: they are
        // output, not terminal-reporting modes.
        let keep: &[u8] = b"\x1b[4h\x1b[4l\x1b[1;31m\x1b[m";
        assert_eq!(filtered(&[keep]), keep);
    }

    #[test]
    fn drops_osc_queries_but_keeps_sets() {
        // Colour and palette *queries* end in '?'; sets do not.
        assert_eq!(filtered(&[b"a\x1b]10;?\x07b"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b]10;?\x1b\\b"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b]4;0;?\x07b"]), b"ab");
        assert_eq!(filtered(&[b"a\x1b]1337;Capabilities=?\x07b"]), b"ab");
        let set: &[u8] = b"\x1b]10;rgb:dcaa/dcab/dcaa\x07";
        assert_eq!(filtered(&[set]), set);
        let title: &[u8] = b"\x1b]0;my title\x07";
        assert_eq!(filtered(&[title]), title);
    }

    #[test]
    fn drops_dcs_queries_but_keeps_sixel_like() {
        assert_eq!(filtered(&[b"a\x1bP+q5445\x1b\\b"]), b"ab");
        assert_eq!(filtered(&[b"a\x1bP$qm\x1b\\b"]), b"ab");
        let sixel: &[u8] = b"\x1bP0;1;0q\x1b\\";
        assert_eq!(filtered(&[sixel]), sixel);
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
        assert_eq!(filtered(&[b"a\x1b]1", b"0;?\x07b"]), b"ab");
        assert_eq!(filtered(&[b"a\x1bP", b"+q5445\x1b", b"\\b"]), b"ab");
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
