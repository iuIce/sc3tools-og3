use nom::{
    bytes::complete::is_not, character::complete::anychar, character::complete::char,
    combinator::map, combinator::map_res, combinator::recognize, sequence::delimited, IResult,
};

use crate::gamedef::GameDef;
use std::{borrow::Cow, collections::HashMap, error, fmt};

pub const FULLWIDTH_SPACE: char = '\u{3000}';

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Text<'a>(pub Cow<'a, str>);

#[derive(Eq, PartialEq, Debug, Clone)]
pub enum Char<'a> {
    Regular(char),
    Compound(&'a str),
}

impl<'a> Text<'a> {
    pub fn iter(&self, encoding_maps: &'a EncodingMaps, has_vtext: bool) -> CharIterator<'_> {
        CharIterator {
            remaining: &self.0,
            encoding_maps,
            has_vtext,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn from_chars(
        chars: impl Iterator<Item = Char<'a>>,
        keep_fullwidth_chars: bool,
    ) -> Text<'a> {
        let mut buf = String::new();
        for res in chars {
            match res {
                Char::Regular(mut c) => {
                    if !keep_fullwidth_chars {
                        c = replace_fullwidth(c);
                    }

                    buf.push(c);
                }
                Char::Compound(s) => {
                    buf.push('[');
                    buf.push_str(s);
                    buf.push(']');
                }
            }
        }

        Text(buf.into())
    }
}

pub struct CharIterator<'a> {
    remaining: &'a str,
    encoding_maps: &'a EncodingMaps,
    has_vtext: bool,
}

impl<'a> Iterator for CharIterator<'a> {
    type Item = Char<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() { return None; }
        
        fn next_char<'a>(s: &'a str, encoding_maps: &EncodingMaps, has_vtext: bool) -> IResult<&'a str, Char<'a>> {
            let encode_compound = move |ch| encode_compound_char(ch, encoding_maps);
            let compound_bracketed = |input: &'a str| -> IResult<&'a str, &'a str> {
                delimited(
                    char('['),
                    recognize(map_res(is_not("]"), encode_compound)),
                    char(']'),
                )(input)
            };

            if let Ok((rem, ch)) = compound_bracketed(s) {
                return Ok((rem, Char::Compound(ch)));
            }

            if has_vtext {
                let mut longest_match: Option<&str> = None;
                for (k, _) in encoding_maps.compound.iter() {
                    if s.starts_with(k)
                        && longest_match.is_none_or(|m| k.len() > m.len()) {
                            longest_match = Some(k.as_str());
                        }
                }
                if let Some(m) = longest_match {
                    return Ok((&s[m.len()..], Char::Compound(&s[..m.len()])));
                }
            }

            map(anychar, Char::Regular)(s)
        }

        let res = next_char(self.remaining, self.encoding_maps, self.has_vtext).ok();
        if let Some((rem, ch)) = res {
            self.remaining = rem;
            Some(ch)
        } else {
            None
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum EncodingError {
    IllegalCharCode(u16),
    CharNotInCharset(String),
    PuaCharNotMapped(u16, char),
}

impl error::Error for EncodingError {}

#[derive(Debug)]
pub struct EncodingMapConstructionError {
    pub missing_pua_chars: Vec<char>,
}

pub struct EncodingMaps {
    main: HashMap<char, u16>,
    compound: HashMap<String, u16>,
}

impl EncodingMaps {
    pub fn new(
        charset: &[String],
        pua_mappings: &HashMap<char, String>,
    ) -> Self {
        let mut main = HashMap::new();
        let mut compound = HashMap::new();

        for (i, s) in charset.iter().enumerate() {
            if s.is_empty() {
                continue;
            }
            let high_byte = 0x80u8 + (i / 256) as u8;
            let low_byte = (i % 256) as u8;
            let code = (high_byte as u16) << 8u16 | (low_byte as u16);

            let mut chars = s.chars();
            let first_char = chars.next().unwrap();
            if chars.next().is_none() {
                main.insert(first_char, code);
            } else {
                compound.insert(s.clone(), code);
            }
        }

        let lookup_compound = |ch: &char| main.get(ch).copied().ok_or(*ch);

        let pua_compound: Vec<_> = pua_mappings
            .iter()
            .filter_map(|(k, v)| lookup_compound(k).map(|code| (v.clone(), code)).ok())
            .collect();

        for p in pua_compound {
            compound.insert(p.0, p.1);
        }
        
        EncodingMaps { main, compound }
    }
}

pub fn encode_str(
    s: &Text,
    gamedef: &GameDef,
    convert_to_fullwidth: bool,
    has_vtext: bool,
) -> Result<Vec<u16>, EncodingError> {
    let mut buf = Vec::new();
    for mut ch in s.iter(&gamedef.encoding_maps, has_vtext) {
        if let Char::Regular(c) = &ch {
            if convert_to_fullwidth && !gamedef.fullwidth_blocklist.contains(c) {
                ch = Char::Regular(replace_halfwidth(*c));
            } else if *c == '\u{20}' {
                ch = Char::Regular(FULLWIDTH_SPACE);
            }
        }
        buf.push(encode_char(&ch, gamedef)?);
    }

    Ok(buf)
}

fn encode_char(ch: &Char, gamedef: &GameDef) -> Result<u16, EncodingError> {
    match ch {
        Char::Compound(s) => encode_compound_char(s, &gamedef.encoding_maps),
        Char::Regular(c) => encode_regular_char(*c, &gamedef.encoding_maps),
    }
}

fn encode_regular_char(c: char, encoding_maps: &EncodingMaps) -> Result<u16, EncodingError> {
    encoding_maps
        .main
        .get(&c)
        .cloned()
        .ok_or_else(|| EncodingError::CharNotInCharset(c.to_string()))
}

fn encode_compound_char(ch: &str, encoding_maps: &EncodingMaps) -> Result<u16, EncodingError> {
    encoding_maps
        .compound
        .get(ch)
        .cloned()
        .ok_or_else(|| EncodingError::CharNotInCharset(ch.to_string()))
}

pub fn decode_str<'a>(
    s: &[u16],
    gamedef: &'a GameDef,
    keep_fullwidth_chars: bool,
) -> Result<Text<'a>, EncodingError> {
    let chars = s
        .iter()
        .map(|code| decode_char(*code, gamedef.charset(), &gamedef.compound_chars))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Text::from_chars(chars.into_iter(), keep_fullwidth_chars))
}

pub fn decode_char<'a>(
    code: u16,
    charset: &'a [String],
    compound_map: &'a HashMap<char, String>,
) -> Result<Char<'a>, EncodingError> {
    let i = (code & 0x7FFF) as usize;
    let s = charset
        .get(i)
        .ok_or(EncodingError::IllegalCharCode(code))?;
    
    let mut chars = s.chars();
    if let Some(ch) = chars.next() {
        if chars.next().is_none() {
            if let '\u{e000}'..='\u{f8ff}' = ch {
                
                return compound_map
                    .get(&ch)
                    .map(|mapped_str| Char::Compound(mapped_str))
                    .ok_or(EncodingError::PuaCharNotMapped(code, ch));
            } else {
                return Ok(Char::Regular(ch));
            }
        }
    }
    
    Ok(Char::Compound(s))
}

pub fn to_halfwidth<'a>(s: &'a Text, encoding_maps: &'a EncodingMaps) -> Text<'a> {
    Text::from_chars(s.iter(encoding_maps, false), false)
}

pub fn is_fullwidth_ch(ch: char) -> bool {
    ('\u{ff00}'..='\u{ff7f}').contains(&ch)
}

fn replace_halfwidth(ch: char) -> char {
    match ch {
        '\u{20}' => FULLWIDTH_SPACE,
        '\u{21}'..='\u{007f}' => std::char::from_u32(ch as u32 + 0xfee0u32).unwrap(),
        _ => ch,
    }
}

pub fn replace_fullwidth(ch: char) -> char {
    match ch {
        '\u{ff00}'..='\u{ff7f}' => std::char::from_u32(ch as u32 - 0xfee0u32).unwrap(),
        FULLWIDTH_SPACE => '\u{20}',
        _ => ch,
    }
}

impl fmt::Display for EncodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodingError::IllegalCharCode(code) => {
                write!(f, "illegal character code ({:#X})", code)
            }
            EncodingError::CharNotInCharset(ch) => {
                write!(f, "character '{}' is not present in the charset", ch)
            }
            EncodingError::PuaCharNotMapped(code, ch) => write!(
                f,
                "{:#X} corresponds to a private use area character '{}' which isn't properly mapped.",
                code,
                ch.escape_unicode()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamedef;

    static SG0_DEF_JSON: &str = r#"
    [{
        "name": "Steins;Gate 0",
        "resource_dir": "oregairu",
        "aliases": ["oregairu", "steinsgate0"],
        "reserved_codepoints": {
        "start": "\uE12F",
        "end": "\uE2AF"
        },
        "fullwidth_blocklist": ["'", "-", "[", "]", "(", ")"]
    }]"#;

    static DEFS: std::sync::LazyLock<Vec<gamedef::GameDef>> = std::sync::LazyLock::new(|| gamedef::build_gamedefs_from_json(SG0_DEF_JSON));

    #[test]
    fn char_iter_regular() {
        let gamedef: &GameDef = gamedef::get_by_alias(&DEFS, "oregairu").unwrap();
        let text = Text(Cow::from("A"));
        let ch = text.iter(&gamedef.encoding_maps, false).next().unwrap();
        assert_eq!(ch, Char::Regular('A'));
    }

    #[test]
    fn char_iter_compound() {
        let gamedef: &GameDef = gamedef::get_by_alias(&DEFS, "oregairu").unwrap();
        let text = Text(Cow::from("[ü]"));
        let ch = text.iter(&gamedef.encoding_maps, false).next().unwrap();
        assert_eq!(ch, Char::Compound("ü"));
    }

    #[test]
    fn encode_roundtrip_regular() {
        let gamedef: &GameDef = gamedef::get_by_alias(&DEFS, "oregairu").unwrap();
        let ch = Char::Regular('A');
        let code = encode_char(&ch, &gamedef).unwrap();
        let decoded = decode_char(code, gamedef.charset(), &gamedef.compound_chars);
        assert_eq!(decoded, Ok(ch));
    }

    #[test]
    fn encode_roundtrip_compound() {
        let gamedef: &GameDef = gamedef::get_by_alias(&DEFS, "oregairu").unwrap();
        let ch = Char::Compound("ü");
        let code = encode_char(&ch, &gamedef).unwrap();
        let decoded = decode_char(code, gamedef.charset(), &gamedef.compound_chars);
        assert_eq!(decoded, Ok(ch));
    }

    #[test]
    fn decode_invalid() {
        let gamedef: &GameDef = gamedef::get_by_alias(&DEFS, "oregairu").unwrap();
        let code = 52768u16;
        let res = decode_char(code, gamedef.charset(), &gamedef.compound_chars);
        assert!(res.is_err());
    }
}

