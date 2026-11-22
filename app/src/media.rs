//! A game's home-screen look: its background picture and menu music
//! (`sce_sys/snd0.at9`, ATRAC9 in a RIFF WAVE, decoded with atrac9dec).

/// Decoded menu music: interleaved samples in -1..1.
#[derive(Clone)]
pub struct Music {
    pub channels: u16,
    pub rate: u32,
    pub samples: Vec<f32>,
}

/// Background pictures are scaled down to this width; they sit behind the UI.
const BACKGROUND_WIDTH: u32 = 1280;

/// The background picture, scaled down, as RGBA.
pub fn decode_background(png: &[u8]) -> Option<image::RgbaImage> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let img = if img.width() > BACKGROUND_WIDTH { img.thumbnail(BACKGROUND_WIDTH, u32::MAX) } else { img };
    Some(img.to_rgba8())
}

/// Decode an `.at9` file. `None` if it isn't ATRAC9 or doesn't decode.
pub fn decode_at9(data: &[u8]) -> Option<Music> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let (mut config, mut block_align, mut body) = (None, 0usize, None);
    let mut pos = 12;
    while pos + 8 <= data.len() {
        let size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().ok()?) as usize;
        let chunk = &data[pos + 8..(pos + 8 + size).min(data.len())];
        match &data[pos..pos + 4] {
            // WAVEFORMATEXTENSIBLE; the 4-byte ATRAC9 config follows the GUID and version.
            b"fmt " if chunk.len() >= 48 => {
                block_align = u16::from_le_bytes([chunk[12], chunk[13]]) as usize;
                config = Some(<[u8; 4]>::try_from(&chunk[44..48]).ok()?);
            }
            b"data" => body = Some(chunk),
            _ => {}
        }
        pos += 8 + size + (size & 1);
    }
    let (config, body) = (config.filter(|c| c[0] == 0xFE)?, body?);
    let mut decoder = atrac9dec::Atrac9Decoder::new(&config).ok()?;
    let info = decoder.codec_info();
    if block_align == 0 {
        return None;
    }
    let mut frame = vec![0i16; info.channels * info.frame_samples * info.frames_in_superframe];
    let mut samples = Vec::with_capacity(body.len() / block_align * frame.len());
    for sf in body.chunks_exact(block_align) {
        if decoder.decode(sf, &mut frame).is_err() {
            frame.fill(0);
        }
        samples.extend(frame.iter().map(|&s| s as f32 / 32768.0));
    }
    (!samples.is_empty()).then_some(Music { channels: info.channels as u16, rate: info.sampling_rate, samples })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_at9() {
        assert!(decode_at9(b"not audio").is_none());
        assert!(decode_at9(b"RIFF\x04\0\0\0WAVE").is_none());
    }

    /// Decodes atrac9dec's own sample if it's in the Cargo cache: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn decodes_a_real_at9() {
        let home = std::env::var_os("CARGO_HOME").map(std::path::PathBuf::from).or_else(|| {
            std::env::var_os("USERPROFILE").or(std::env::var_os("HOME")).map(|h| std::path::Path::new(&h).join(".cargo"))
        });
        let src = home.unwrap().join("registry").join("src");
        let sample = std::fs::read_dir(src).unwrap().flatten().map(|d| d.path().join("atrac9dec-0.9.1/tests/sample.at9")).find(|p| p.is_file()).unwrap();
        let m = decode_at9(&std::fs::read(sample).unwrap()).unwrap();
        assert_eq!((m.channels, m.rate), (2, 48000));
        assert_eq!(m.samples.len(), 954_368);
    }
}
