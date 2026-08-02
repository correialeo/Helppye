//! Ponto de extensão de transcrição: `TranscriptionProvider`.
//!
//! O contrato é **por sessão**, não por segmento. A forma anterior (`transcribe(segment)
//! -> Transcript`, hoje `SegmentTranscriber`) descrevia bem um engine batch local e
//! descrevia mal qualquer outra coisa: um backend de streaming não recebe segmentos
//! prontos, emite resultados parciais antes do final, e tem um ciclo de vida próprio
//! (abrir, alimentar, encerrar) que precisa acompanhar a fronteira de sessão de conversa.
//! Forçar esse backend no molde batch significaria acumular áudio para fingir segmentos e
//! jogar fora os parciais — perdendo justamente a latência que motiva usá-lo.
//!
//! `capabilities()` existe para que a diferença entre backends seja **declarada**, e não
//! descoberta em runtime: a UI e o runtime consultam as capacidades em vez de assumir que
//! todo provider tem parciais, ou que todo provider funciona offline.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::transcription::error::TranscriptionError;
use crate::transcription::session::{TranscriptionSession, TranscriptionSessionContext};

/// Identidade estável de um backend de transcrição. É o que a configuração persiste e o
/// registry indexa — nunca o nome de exibição, que é texto de UI e pode mudar.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionProviderId {
    /// Whisper local via whisper.cpp. Padrão, e o único implementado hoje.
    #[default]
    WhisperLocal,
    /// OpenAI Realtime Transcription. Contrato previsto, **não implementado** — ver
    /// `docs/transcription-providers.md`.
    OpenAiRealtime,
    /// Google Gemini. Contrato previsto, **não implementado**.
    GoogleGemini,
    /// Endpoint compatível com a API da OpenAI, informado pelo usuário. Contrato previsto,
    /// **não implementado**.
    OpenAiCompatible,
    /// Provider controlado, só para testes. Nunca registrado em produção.
    Fake,
}

impl TranscriptionProviderId {
    pub fn as_str(self) -> &'static str {
        match self {
            TranscriptionProviderId::WhisperLocal => "whisper_local",
            TranscriptionProviderId::OpenAiRealtime => "openai_realtime",
            TranscriptionProviderId::GoogleGemini => "google_gemini",
            TranscriptionProviderId::OpenAiCompatible => "openai_compatible",
            TranscriptionProviderId::Fake => "fake",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            TranscriptionProviderId::WhisperLocal => "Whisper local",
            TranscriptionProviderId::OpenAiRealtime => "OpenAI Realtime",
            TranscriptionProviderId::GoogleGemini => "Gemini Live",
            TranscriptionProviderId::OpenAiCompatible => "Endpoint compatível com OpenAI",
            TranscriptionProviderId::Fake => "Provider de teste",
        }
    }
}

impl std::fmt::Display for TranscriptionProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TranscriptionProviderId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "whisper_local" => Ok(Self::WhisperLocal),
            "openai_realtime" => Ok(Self::OpenAiRealtime),
            "google_gemini" => Ok(Self::GoogleGemini),
            "openai_compatible" => Ok(Self::OpenAiCompatible),
            "fake" => Ok(Self::Fake),
            _ => Err(format!("unknown transcription provider: {value}")),
        }
    }
}

/// O que um backend consegue fazer, declarado explicitamente. Cada campo existe porque
/// alguma decisão real depende dele:
///
/// - `local`: se `false`, áudio da reunião sai da máquina — precisa de consentimento
///   explícito, não pode ser default silencioso.
/// - `streaming` / `partial_results`: o runtime só espera parciais de quem declara
///   produzi-los; parcial nunca vira segmento da timeline.
/// - `speaker_source_preserved`: um backend que misturasse fontes seria inutilizável aqui —
///   distinguir microfone de saída de sistema é o que define quem falou.
/// - `language_selection` / `automatic_language_detection`: o que a tela de idioma pode
///   oferecer sem prometer o que o backend não faz.
/// - `requires_credentials`: se `true`, precisa de API key no keychain antes de iniciar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptionCapabilities {
    pub local: bool,
    pub streaming: bool,
    pub partial_results: bool,
    pub speaker_source_preserved: bool,
    pub language_selection: bool,
    pub automatic_language_detection: bool,
    pub requires_credentials: bool,
}

impl TranscriptionCapabilities {
    /// Base conservadora: nada é assumido como suportado além de preservar a fonte, que é
    /// requisito de arquitetura e não uma capacidade opcional.
    pub const fn none() -> Self {
        TranscriptionCapabilities {
            local: false,
            streaming: false,
            partial_results: false,
            speaker_source_preserved: true,
            language_selection: false,
            automatic_language_detection: false,
            requires_credentials: false,
        }
    }
}

#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    fn id(&self) -> TranscriptionProviderId;

    fn capabilities(&self) -> TranscriptionCapabilities;

    /// Abre uma sessão para **uma** fonte de áudio. O runtime chama uma vez por fonte por
    /// sessão de conversa; nunca reaproveita uma sessão entre fronteiras de sessão.
    ///
    /// Falhar aqui é a forma correta de um provider indisponível se comportar (modelo não
    /// carregado, credencial ausente, endpoint inválido). Um provider nunca deve abrir uma
    /// sessão que sabidamente não vai transcrever nada.
    async fn start_session(
        &self,
        context: TranscriptionSessionContext,
    ) -> Result<Box<dyn TranscriptionSession>, TranscriptionError>;

    /// Verificação barata de prontidão, sem abrir sessão — usada por diagnósticos e pela
    /// UI. O default assume pronto; providers com pré-requisito (modelo carregado,
    /// credencial) sobrescrevem.
    async fn readiness(&self) -> Result<(), TranscriptionError> {
        Ok(())
    }
}
