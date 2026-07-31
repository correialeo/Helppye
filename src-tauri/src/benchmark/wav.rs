//! Leitor de WAV mínimo, só o suficiente para o harness de benchmark.
//!
//! Não é uma dependência nova de propósito: o app não precisa ler arquivos de áudio em
//! nenhum caminho de produção — só o harness precisa, e só para PCM 16-bit ou float 32-bit,
//! que é o que qualquer gravador produz. Trazer um crate de decodificação para o binário
//! final por causa de uma ferramenta de medição seria pagar peso permanente por uso
//! ocasional.
//!
//! A conversão para mono 16 kHz reaproveita `audio::resampler`, o mesmo código que a captura
//! usa — se o benchmark reamostrasse de forma diferente do pipeline real, estaria medindo
//! outro sistema.

use std::path::Path;

use crate::audio::resampler::{downmix_to_mono, resample_linear};

#[derive(Debug)]
pub struct DecodedAudio {
    /// Mono, na taxa original do arquivo.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub duration_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum WavError {
    #[error("não foi possível ler {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("arquivo não é um WAV RIFF válido")]
    NotRiff,
    #[error("chunk obrigatório ausente: {0}")]
    MissingChunk(&'static str),
    #[error("formato não suportado pelo harness: {0}")]
    Unsupported(String),
}

const FORMAT_PCM: u16 = 1;
const FORMAT_IEEE_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

pub fn read_wav(path: &Path) -> Result<DecodedAudio, WavError> {
    let bytes = std::fs::read(path).map_err(|source| WavError::Io {
        path: path.display().to_string(),
        source,
    })?;
    decode(&bytes)
}

fn decode(bytes: &[u8]) -> Result<DecodedAudio, WavError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotRiff);
    }

    let mut cursor = 12usize;
    let mut format: Option<(u16, u16, u32, u16)> = None; // (formato, canais, taxa, bits)
    let mut data: Option<&[u8]> = None;

    // Percorre os chunks em vez de assumir que `fmt ` vem imediatamente antes de `data`:
    // gravadores inserem `LIST`/`fact` no meio, e assumir a ordem é o bug clássico de leitor
    // de WAV escrito às pressas.
    while cursor + 8 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = u32::from_le_bytes(read4(bytes, cursor + 4)?) as usize;
        let body_start = cursor + 8;
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = &bytes[body_start..body_end];

        match id {
            b"fmt " => {
                if body.len() < 16 {
                    return Err(WavError::Unsupported("chunk fmt truncado".into()));
                }
                let tag = u16::from_le_bytes([body[0], body[1]]);
                let channels = u16::from_le_bytes([body[2], body[3]]);
                let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                let bits = u16::from_le_bytes([body[14], body[15]]);
                // WAVE_FORMAT_EXTENSIBLE guarda o formato real no sub-GUID; os dois primeiros
                // bytes dele são o tag clássico.
                let tag = if tag == FORMAT_EXTENSIBLE && body.len() >= 26 {
                    u16::from_le_bytes([body[24], body[25]])
                } else {
                    tag
                };
                format = Some((tag, channels, rate, bits));
            }
            b"data" => data = Some(body),
            _ => {}
        }

        // Chunks têm padding para tamanho par.
        cursor = body_start + size + (size % 2);
    }

    let (tag, channels, sample_rate, bits) = format.ok_or(WavError::MissingChunk("fmt "))?;
    let data = data.ok_or(WavError::MissingChunk("data"))?;
    if channels == 0 {
        return Err(WavError::Unsupported("zero canais".into()));
    }

    let interleaved = match (tag, bits) {
        (FORMAT_PCM, 16) => data
            .chunks_exact(2)
            .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / f32::from(i16::MAX))
            .collect::<Vec<f32>>(),
        (FORMAT_PCM, 32) => data
            .chunks_exact(4)
            .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / i32::MAX as f32)
            .collect(),
        (FORMAT_IEEE_FLOAT, 32) => data
            .chunks_exact(4)
            .map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
            .collect(),
        _ => {
            return Err(WavError::Unsupported(format!(
                "formato {tag} com {bits} bits"
            )))
        }
    };

    let samples = downmix_to_mono(&interleaved, channels);
    let duration_ms = if sample_rate == 0 {
        0
    } else {
        (samples.len() as u64 * 1000) / u64::from(sample_rate)
    };

    Ok(DecodedAudio {
        samples,
        sample_rate,
        duration_ms,
    })
}

/// Converte para a taxa que o pipeline usa, reaproveitando o resampler da captura.
pub fn to_target_rate(audio: &DecodedAudio, target_rate: u32) -> Vec<f32> {
    resample_linear(&audio.samples, audio.sample_rate, target_rate)
}

fn read4(bytes: &[u8], at: usize) -> Result<[u8; 4], WavError> {
    bytes
        .get(at..at + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .ok_or(WavError::NotRiff)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Constrói um WAV PCM 16-bit em memória, opcionalmente com um chunk desconhecido antes
    /// de `data` — o caso que quebra leitores que assumem ordem fixa.
    fn wav_pcm16(channels: u16, rate: u32, samples: &[i16], extra_chunk: bool) -> Vec<u8> {
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes()); // PCM
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&rate.to_le_bytes());
        fmt.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        fmt.extend_from_slice(&(channels * 2).to_le_bytes());
        fmt.extend_from_slice(&16u16.to_le_bytes());

        let mut data = Vec::new();
        for s in samples {
            data.extend_from_slice(&s.to_le_bytes());
        }

        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        body.extend_from_slice(&fmt);
        if extra_chunk {
            body.extend_from_slice(b"LIST");
            body.extend_from_slice(&4u32.to_le_bytes());
            body.extend_from_slice(b"INFO");
        }
        body.extend_from_slice(b"data");
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&data);

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn decodes_mono_pcm16() {
        let bytes = wav_pcm16(1, 16_000, &[0, i16::MAX, i16::MIN], false);
        let audio = decode(&bytes).unwrap();
        assert_eq!(audio.sample_rate, 16_000);
        assert_eq!(audio.samples.len(), 3);
        assert!((audio.samples[1] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn downmixes_stereo_to_mono() {
        let bytes = wav_pcm16(2, 48_000, &[i16::MAX, i16::MIN, 0, 0], false);
        let audio = decode(&bytes).unwrap();
        assert_eq!(
            audio.samples.len(),
            2,
            "dois frames estéreo viram dois mono"
        );
        assert!(audio.samples[0].abs() < 1e-3, "canais opostos se cancelam");
    }

    #[test]
    fn tolerates_unknown_chunks_before_data() {
        let bytes = wav_pcm16(1, 16_000, &[1, 2, 3, 4], true);
        let audio = decode(&bytes).unwrap();
        assert_eq!(audio.samples.len(), 4);
    }

    #[test]
    fn rejects_non_riff_input() {
        assert!(matches!(
            decode(b"not a wav at all"),
            Err(WavError::NotRiff)
        ));
    }

    #[test]
    fn reports_unsupported_bit_depth_instead_of_guessing() {
        let mut bytes = wav_pcm16(1, 16_000, &[1, 2], false);
        // Troca a profundidade declarada para 24 bits, que o harness não decodifica.
        let bits_at = bytes
            .windows(4)
            .position(|w| w == b"fmt ")
            .map(|p| p + 8 + 14)
            .unwrap();
        bytes[bits_at..bits_at + 2].copy_from_slice(&24u16.to_le_bytes());
        assert!(matches!(decode(&bytes), Err(WavError::Unsupported(_))));
    }
}
