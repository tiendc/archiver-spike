//! File names as an archive stored them, read in the encoding they were
//! written in.
//!
//! ZIP marks a name as UTF-8 with one flag bit; without it the bytes are in
//! whatever code page the computer that made the archive used (DOS 437 on an
//! American PC, 1258 on a Vietnamese one, Shift_JIS on a Japanese one), and
//! nothing in the file says which. Old RAR and TAR are the same. So the names
//! of one archive are guessed together, as one text, which gives the
//! detector far more to go on than any single short name.

use encoding_rs::Encoding;
use unicode_normalization::UnicodeNormalization;

/// How an archive's names are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    /// UTF-8 (also all-ASCII names).
    Utf8,
    /// The DOS code page the ZIP format names as its default.
    Cp437,
    /// A code page encoding_rs knows (windows-125x, Shift_JIS, GBK, EUC-KR,
    /// Big5, IBM866…).
    Other(&'static Encoding),
}

impl Charset {
    pub fn name(self) -> &'static str {
        match self {
            Charset::Utf8 => "UTF-8",
            Charset::Cp437 => "IBM437",
            Charset::Other(e) => e.name(),
        }
    }

    /// Reads one stored name, composed (NFC). Bytes that do not fit the
    /// charset come out as U+FFFD.
    pub fn decode(self, raw: &[u8]) -> String {
        let text = match self {
            Charset::Utf8 => String::from_utf8_lossy(raw).into_owned(),
            Charset::Cp437 => raw.iter().map(|&b| cp437(b)).collect(),
            Charset::Other(e) => e.decode_without_bom_handling(raw).0.into_owned(),
        };
        text.nfc().collect()
    }
}

/// A guess, and whether it is safe to take without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guess {
    pub charset: Charset,
    /// UTF-8 that decoded cleanly, or names that are all ASCII: nothing to
    /// ask. Anything detected is shown with a preview the user can change
    /// ("Do the file names look right?").
    pub certain: bool,
}

/// What is known about where the archive was made, from the user's side:
/// their language's country, as a web domain (`vn`, `jp`, `cn`), which
/// tips the detector between close code pages the way a site's domain does.
#[derive(Debug, Clone, Copy, Default)]
pub struct Hint<'a> {
    pub country_tld: Option<&'a str>,
}

/// Guesses the charset of one archive's names, all at once.
pub fn guess<'n>(names: impl IntoIterator<Item = &'n [u8]>, hint: Hint<'_>) -> Guess {
    let names: Vec<&[u8]> = names.into_iter().filter(|n| !n.is_ascii()).collect();
    if names.is_empty() || names.iter().all(|n| std::str::from_utf8(n).is_ok()) {
        return Guess {
            charset: Charset::Utf8,
            certain: true,
        };
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    for name in &names {
        detector.feed(name, false);
        // Names are separate words; a separator keeps them from running
        // together into byte pairs no encoding would write.
        detector.feed(b"\n", false);
    }
    detector.feed(b"", true);
    let tld = hint.country_tld.map(str::as_bytes);
    let encoding = detector.guess(tld, chardetng::Utf8Detection::Deny);
    let charset = if encoding == encoding_rs::WINDOWS_1252 && dos_reads_better(&names) {
        Charset::Cp437
    } else {
        Charset::Other(encoding)
    };
    Guess {
        charset,
        certain: false,
    }
}

/// The detector knows the web's encodings, not DOS 437, which ZIP made the
/// default and Windows' own "Compressed folder" still writes. Read as
/// windows-1252, 437's letters turn into quotes and symbols ("Gr”áe" for
/// "Größe"), so whichever reads as more letters wins; a tie stays 1252.
fn dos_reads_better(names: &[&[u8]]) -> bool {
    let letters = |text: String| {
        let high: Vec<char> = text.chars().filter(|c| !c.is_ascii()).collect();
        let alpha = high.iter().filter(|c| c.is_alphabetic()).count();
        (alpha, high.len().max(1))
    };
    let joined = names.join(&b'\n');
    let (a1252, n1252) = letters(Charset::Other(encoding_rs::WINDOWS_1252).decode(&joined));
    let (a437, n437) = letters(Charset::Cp437.decode(&joined));
    a437 * n1252 > a1252 * n437
}

/// The DOS code page 437, upper half (the lower half is ASCII).
const CP437_HIGH: &str = concat!(
    "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒ",
    "áíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐",
    "└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀",
    "αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
);

fn cp437(b: u8) -> char {
    if b < 0x80 {
        b as char
    } else {
        CP437_HIGH
            .chars()
            .nth(usize::from(b - 0x80))
            .unwrap_or('\u{fffd}')
    }
}

/// Encodes text as CP437, for test archives; `None` if a character has no
/// place in it.
pub fn encode_cp437(text: &str) -> Option<Vec<u8>> {
    text.chars()
        .map(|c| {
            if c.is_ascii() {
                Some(c as u8)
            } else {
                CP437_HIGH
                    .chars()
                    .position(|h| h == c)
                    .and_then(|i| u8::try_from(0x80 + i).ok())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cp437_table_is_complete_and_round_trips() {
        assert_eq!(CP437_HIGH.chars().count(), 128);
        let text = "Résumé Größe Ñandú ½";
        let bytes = encode_cp437(text).expect("encodable");
        assert_eq!(Charset::Cp437.decode(&bytes), text);
    }

    #[test]
    fn utf8_and_ascii_are_certain() {
        let g = guess([b"plain.txt".as_slice()], Hint::default());
        assert_eq!(
            g,
            Guess {
                charset: Charset::Utf8,
                certain: true
            }
        );
        let g = guess(["Bản vẽ.pdf".as_bytes()], Hint::default());
        assert_eq!(
            g,
            Guess {
                charset: Charset::Utf8,
                certain: true
            }
        );
    }

    #[test]
    fn decomposed_utf8_comes_out_composed() {
        assert_eq!(Charset::Utf8.decode("Ba\u{309}n".as_bytes()), "Bản");
    }
}
