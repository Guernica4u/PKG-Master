//! A game's title, title ID, version and icon, read from `param.sfo` (PS4),
//! `param.json` (PS5) and `icon0.png`.

use std::collections::HashMap;

use crate::binary::{u16le, u32le};

/// What the window shows above the file details. Every field is optional:
/// a file that is missing or malformed just leaves its field empty.
#[derive(Clone, Debug, Default)]
pub struct GameInfo {
    pub title: Option<String>,
    pub title_id: Option<String>,
    pub version: Option<String>,
    pub icon: Option<Icon>,
}

/// A decoded icon as straight (non-premultiplied) RGBA pixels.
#[derive(Clone, Debug)]
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Icons bigger than this are not decoded (PS4/PS5 icons are 512x512).
const MAX_ICON_SIDE: u32 = 2048;
/// Metadata files bigger than this are ignored.
pub const MAX_META_SIZE: u64 = 16 << 20;

impl GameInfo {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.title_id.is_none() && self.icon.is_none()
    }

    /// Build from the raw files; each one is optional.
    pub fn from_files(sfo: Option<&[u8]>, json: Option<&[u8]>, icon_png: Option<&[u8]>) -> Self {
        let mut info = GameInfo::default();
        if let Some(json) = json {
            info.fill_from_json(json);
        }
        if let Some(sfo) = sfo {
            let values = parse_sfo(sfo);
            let get = |k: &str| values.get(k).filter(|v| !v.is_empty()).cloned();
            info.title = info.title.or_else(|| get("TITLE"));
            info.title_id = info.title_id.or_else(|| get("TITLE_ID"));
            info.version = info.version.or_else(|| get("APP_VER"));
        }
        info.icon = icon_png.and_then(decode_icon);
        info
    }

    /// Fill only the fields that are still empty.
    pub fn merge(&mut self, other: GameInfo) {
        self.title = self.title.take().or(other.title);
        self.title_id = self.title_id.take().or(other.title_id);
        self.version = self.version.take().or(other.version);
        self.icon = self.icon.take().or(other.icon);
    }

    fn fill_from_json(&mut self, data: &[u8]) {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else { return };
        let text = |v: &serde_json::Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
        self.title_id = v.get("titleId").and_then(text);
        self.version = v.get("contentVersion").or_else(|| v.get("masterVersion")).and_then(text);
        if let Some(loc) = v.get("localizedParameters") {
            let lang = loc.get("defaultLanguage").and_then(|l| l.as_str()).unwrap_or("en-US");
            let entry = loc
                .get(lang)
                .or_else(|| loc.get("en-US"))
                .or_else(|| loc.as_object().and_then(|o| o.values().find(|e| e.is_object())));
            self.title = entry.and_then(|e| e.get("titleName")).and_then(text);
        }
    }
}

/// The string and integer values of a PS4 `param.sfo` (PSF) file. Integers
/// are returned in decimal. Anything malformed is skipped.
pub fn parse_sfo(d: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if d.len() < 20 || &d[..4] != b"\0PSF" {
        return out;
    }
    let key_table = u32le(d, 8) as usize;
    let data_table = u32le(d, 12) as usize;
    let count = u32le(d, 16) as usize;
    for i in 0..count.min(1024) {
        let e = 20 + i * 16;
        if e + 16 > d.len() {
            break;
        }
        let key_off = key_table + u16le(d, e) as usize;
        let fmt = u16le(d, e + 2);
        let len = u32le(d, e + 4) as usize;
        let data_off = data_table + u32le(d, e + 12) as usize;
        let Some(key_bytes) = d.get(key_off..) else { continue };
        let key_end = key_bytes.iter().position(|&b| b == 0).unwrap_or(key_bytes.len());
        let key = String::from_utf8_lossy(&key_bytes[..key_end]).into_owned();
        let Some(raw) = d.get(data_off..data_off.saturating_add(len)) else { continue };
        let value = match fmt {
            // UTF-8 string, NUL-terminated or not.
            0x0004 | 0x0204 => {
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                String::from_utf8_lossy(&raw[..end]).trim().to_string()
            }
            0x0404 if raw.len() >= 4 => u32le(raw, 0).to_string(),
            _ => continue,
        };
        out.insert(key, value);
    }
    out
}

/// Decode a PNG icon, or `None` if it isn't a usable PNG.
pub fn decode_icon(png: &[u8]) -> Option<Icon> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    if img.width() == 0 || img.height() == 0 || img.width() > MAX_ICON_SIDE || img.height() > MAX_ICON_SIDE {
        return None;
    }
    let rgba = img.to_rgba8();
    Some(Icon { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal PSF with TITLE, TITLE_ID, APP_VER (strings) and an integer.
    fn sfo() -> Vec<u8> {
        let entries: [(&str, u16, &[u8]); 4] = [
            ("APP_VER", 0x0204, b"01.00\0"),
            ("ATTRIBUTE", 0x0404, &[5, 0, 0, 0]),
            ("TITLE", 0x0204, b"Bad North\0"),
            ("TITLE_ID", 0x0204, b"CUSA10969\0"),
        ];
        let mut keys = Vec::new();
        let mut data = Vec::new();
        let mut index = Vec::new();
        for (k, fmt, v) in entries {
            index.push((keys.len() as u16, fmt, v.len() as u32, data.len() as u32));
            keys.extend_from_slice(k.as_bytes());
            keys.push(0);
            data.extend_from_slice(v);
        }
        let key_table = 20 + index.len() * 16;
        let data_table = key_table + keys.len();
        let mut out = b"\0PSF".to_vec();
        out.extend_from_slice(&0x0101u32.to_le_bytes());
        out.extend_from_slice(&(key_table as u32).to_le_bytes());
        out.extend_from_slice(&(data_table as u32).to_le_bytes());
        out.extend_from_slice(&(index.len() as u32).to_le_bytes());
        for (ko, fmt, len, doff) in index {
            out.extend_from_slice(&ko.to_le_bytes());
            out.extend_from_slice(&fmt.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&doff.to_le_bytes());
        }
        out.extend_from_slice(&keys);
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn reads_param_sfo() {
        let info = GameInfo::from_files(Some(&sfo()), None, None);
        assert_eq!(info.title.as_deref(), Some("Bad North"));
        assert_eq!(info.title_id.as_deref(), Some("CUSA10969"));
        assert_eq!(info.version.as_deref(), Some("01.00"));
        assert_eq!(parse_sfo(&sfo()).get("ATTRIBUTE").map(String::as_str), Some("5"));
    }

    #[test]
    fn reads_param_json_default_language() {
        let json = br#"{"titleId":"PPSA01234","contentVersion":"01.000.000",
            "localizedParameters":{"defaultLanguage":"fr-FR",
              "en-US":{"titleName":"Game"},"fr-FR":{"titleName":"Jeu"}}}"#;
        let info = GameInfo::from_files(None, Some(json), None);
        assert_eq!(info.title.as_deref(), Some("Jeu"));
        assert_eq!(info.title_id.as_deref(), Some("PPSA01234"));
        assert_eq!(info.version.as_deref(), Some("01.000.000"));
    }

    #[test]
    fn junk_is_ignored() {
        let info = GameInfo::from_files(Some(b"junk"), Some(b"{not json"), Some(b"not a png"));
        assert!(info.is_empty());
    }

    #[test]
    fn decodes_png_icon() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 2, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let icon = decode_icon(&png).unwrap();
        assert_eq!((icon.width, icon.height), (4, 2));
        assert_eq!(&icon.rgba[..4], &[1, 2, 3, 255]);
    }
}
