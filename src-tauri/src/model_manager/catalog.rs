//! Catálogo centralizado dos modelos de transcrição oferecidos para download guiado.
//! Nenhuma URL, nome de arquivo, tamanho ou hash deve aparecer fora deste módulo — ver
//! `docs/local-transcription.md`.

/// Idiomas suportados por um modelo de transcrição.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelLanguageSupport {
    /// Multilíngue (ex.: variantes "base"/"small" sem sufixo `.en`).
    Multilingual,
    /// Apenas inglês (sufixo `.en`) — nunca usado como padrão nesta aplicação, cujo
    /// idioma inicial é português.
    EnglishOnly,
}

/// Definição estática de um modelo baixável. `sha256` e `approximate_size_bytes` são
/// valores reais, obtidos baixando o arquivo oficial e computando o checksum localmente
/// (nunca estimados ou copiados de um header HTTP não confiável, como `x-linked-size`
/// ou `x-xet-hash`).
#[derive(Debug, Clone, Copy)]
pub struct ModelDefinition {
    pub id: &'static str,
    pub display_name: &'static str,
    pub filename: &'static str,
    pub download_url: &'static str,
    pub sha256: &'static str,
    pub approximate_size_bytes: u64,
    pub language_support: ModelLanguageSupport,
}

/// Modelo padrão oferecido no fluxo de download guiado: Whisper Base Multilíngue
/// (`ggml-base.bin`, `ggerganov/whisper.cpp`). Explicitamente NÃO é `base.en` — o
/// idioma inicial da aplicação é português, e variantes `.en` são exclusivas de inglês.
///
/// `sha256` e `approximate_size_bytes` foram obtidos baixando o arquivo real de
/// `download_url` e computando `sha256sum`/tamanho sobre os bytes recebidos.
pub const DEFAULT_MODEL: ModelDefinition = ModelDefinition {
    id: "whisper-base-multilingual",
    display_name: "Whisper Base Multilíngue",
    filename: "ggml-base.bin",
    download_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
    sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    approximate_size_bytes: 147_951_465,
    language_support: ModelLanguageSupport::Multilingual,
};
