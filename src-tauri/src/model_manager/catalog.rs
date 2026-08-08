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

impl ModelLanguageSupport {
    /// Deduz o suporte de idioma pelo nome do arquivo. É best-effort e existe por um único
    /// motivo: um `.bin` do whisper.cpp não carrega metadado de idioma, então o sufixo `.en`
    /// da convenção oficial é o único sinal disponível sobre um modelo personalizado
    /// escolhido pelo usuário. Um arquivo renomeado engana a dedução — o resultado é um
    /// rótulo errado na tela, nunca uma decisão de transcrição.
    pub fn from_model_filename(filename: &str) -> Self {
        let lower = filename.to_ascii_lowercase();
        if lower.contains(".en.") || lower.ends_with(".en") {
            ModelLanguageSupport::EnglishOnly
        } else {
            ModelLanguageSupport::Multilingual
        }
    }
}

/// Definição estática de um modelo baixável. `sha256` e `approximate_size_bytes` são
/// valores reais, obtidos baixando o arquivo oficial e computando o checksum localmente
/// (nunca estimados ou copiados de um header HTTP não confiável, como `x-linked-size`
/// ou `x-xet-hash`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Modelo Whisper Large-v3 Turbo, oferecido como alternativa local para comparação
/// com providers de streaming como o Gemini Live. O `x-linked-etag` do Hugging Face
/// é o SHA-256 do arquivo original (o `etag` do Xet é apenas o identificador da
/// reconstrução e não serve para a verificação do conteúdo).
pub const WHISPER_TURBO_MODEL: ModelDefinition = ModelDefinition {
    id: "whisper-large-v3-turbo",
    display_name: "Whisper Large-v3 Turbo",
    filename: "ggml-large-v3-turbo.bin",
    download_url:
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
    sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
    approximate_size_bytes: 1_624_555_275,
    language_support: ModelLanguageSupport::Multilingual,
};

pub const MANAGED_MODELS: &[ModelDefinition] = &[DEFAULT_MODEL, WHISPER_TURBO_MODEL];

pub fn find_managed_model(model_id: &str) -> Option<ModelDefinition> {
    MANAGED_MODELS
        .iter()
        .copied()
        .find(|model| model.id == model_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guarda o que o comentário de `DEFAULT_MODEL` afirma. Trocar `ggml-base.bin` por
    /// `ggml-base.en.bin` é uma edição de uma linha que passaria despercebida em revisão
    /// e faria toda reunião em português sair transcrita como inglês fonético.
    #[test]
    fn the_default_model_is_multilingual_never_english_only() {
        assert_eq!(
            DEFAULT_MODEL.language_support,
            ModelLanguageSupport::Multilingual
        );
        assert_ne!(
            DEFAULT_MODEL.language_support,
            ModelLanguageSupport::EnglishOnly
        );
        assert_eq!(
            ModelLanguageSupport::from_model_filename(DEFAULT_MODEL.filename),
            ModelLanguageSupport::Multilingual,
            "variante .en é exclusiva de inglês: {}",
            DEFAULT_MODEL.filename
        );
    }

    #[test]
    fn english_only_models_are_recognized_by_the_en_suffix() {
        for filename in ["ggml-base.en.bin", "ggml-small.en.bin", "GGML-TINY.EN.BIN"] {
            assert_eq!(
                ModelLanguageSupport::from_model_filename(filename),
                ModelLanguageSupport::EnglishOnly,
                "{filename}"
            );
        }
        for filename in ["ggml-base.bin", "ggml-large-v3.bin", "meu-modelo.bin"] {
            assert_eq!(
                ModelLanguageSupport::from_model_filename(filename),
                ModelLanguageSupport::Multilingual,
                "{filename}"
            );
        }
    }

    #[test]
    fn turbo_model_is_multilingual_and_registered() {
        assert_eq!(
            WHISPER_TURBO_MODEL.language_support,
            ModelLanguageSupport::Multilingual
        );
        assert_eq!(
            find_managed_model(WHISPER_TURBO_MODEL.id),
            Some(WHISPER_TURBO_MODEL)
        );
    }
}
