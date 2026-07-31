//! Registry de backends de geração de resposta.
//!
//! Simétrico a `transcription::registry`, e pelo mesmo motivo: "quais provedores existem,
//! quais funcionam nesta build, e o que cada um sabe fazer" precisa ser um dado consultável
//! — pela UI, por diagnóstico e por log — e não conhecimento espalhado por um `match` em
//! `build_provider`.
//!
//! A diferença em relação ao registry de transcrição é que um provedor de geração não é uma
//! instância única de longa vida: ele é reconstruído a cada troca de configuração (modelo
//! novo, endpoint novo, chave nova). Por isso aqui não se guardam instâncias, e sim
//! **descritores**: identidade, capacidades declaradas e disponibilidade. Guardar instâncias
//! obrigaria a invalidá-las a cada `update_config` e abriria a porta para uma geração sair
//! por um provider com a configuração antiga.

use serde::Serialize;

use super::provider::{ResponseProviderCapabilities, ResponseProviderId};

#[derive(Debug, Clone, Serialize)]
pub struct ResponseProviderDescriptor {
    pub id: ResponseProviderId,
    pub display_name: &'static str,
    pub capabilities: ResponseProviderCapabilities,
    pub available: bool,
    pub unavailable_reason: Option<&'static str>,
}

/// Backends que esta build constrói de fato. `local` aqui é o caso **padrão** de cada um
/// (Ollama e LM Studio em `localhost`); um `base_url` apontando para outra máquina muda a
/// resposta, e é por isso que a instância também declara `capabilities()` — este catálogo
/// descreve o provedor, a instância descreve a configuração dele.
const IMPLEMENTED: &[(ResponseProviderId, ResponseProviderCapabilities)] = &[
    (
        ResponseProviderId::Ollama,
        ResponseProviderCapabilities {
            local: true,
            streaming: true,
            requires_credentials: false,
            configurable_base_url: true,
            custom_headers: false,
        },
    ),
    (
        ResponseProviderId::LmStudio,
        ResponseProviderCapabilities {
            local: true,
            streaming: true,
            requires_credentials: false,
            configurable_base_url: true,
            custom_headers: true,
        },
    ),
    (
        ResponseProviderId::OpenAi,
        ResponseProviderCapabilities {
            local: false,
            streaming: true,
            requires_credentials: true,
            configurable_base_url: true,
            custom_headers: true,
        },
    ),
    (
        ResponseProviderId::DeepSeek,
        ResponseProviderCapabilities {
            local: false,
            streaming: true,
            requires_credentials: true,
            configurable_base_url: true,
            custom_headers: true,
        },
    ),
    (
        ResponseProviderId::Anthropic,
        ResponseProviderCapabilities {
            local: false,
            streaming: true,
            requires_credentials: true,
            configurable_base_url: true,
            custom_headers: false,
        },
    ),
    (
        ResponseProviderId::OpenRouter,
        ResponseProviderCapabilities {
            local: false,
            streaming: true,
            requires_credentials: true,
            configurable_base_url: true,
            custom_headers: true,
        },
    ),
    (
        ResponseProviderId::CustomOpenAiCompatible,
        ResponseProviderCapabilities {
            local: false,
            streaming: true,
            requires_credentials: false,
            configurable_base_url: true,
            custom_headers: true,
        },
    ),
];

/// Previsto, contratualmente representável, e **não** implementado. Aparece na UI como
/// indisponível com o motivo real — esconder produziria a pergunta "cadê o ChatGPT?" sem
/// resposta, e listar como disponível produziria uma sessão inteira sem sugestão nenhuma.
const PLANNED: &[(ResponseProviderId, &str)] = &[(
    ResponseProviderId::ChatGptCodexAccount,
    "não implementado: a autenticação por assinatura ChatGPT Plus/Pro não tem suporte \
oficial documentado para aplicações de terceiros (ver docs/adr/chatgpt-codex-subscription-auth.md)",
)];

/// Tudo que a UI precisa para montar a lista de provedores.
pub fn descriptors() -> Vec<ResponseProviderDescriptor> {
    let mut out: Vec<ResponseProviderDescriptor> = IMPLEMENTED
        .iter()
        .map(|(id, capabilities)| ResponseProviderDescriptor {
            id: *id,
            display_name: id.display_name(),
            capabilities: *capabilities,
            available: true,
            unavailable_reason: None,
        })
        .collect();

    for (id, reason) in PLANNED {
        out.push(ResponseProviderDescriptor {
            id: *id,
            display_name: id.display_name(),
            // Não afirmar capacidade de algo que esta build não entrega.
            capabilities: ResponseProviderCapabilities::none(),
            available: false,
            unavailable_reason: Some(reason),
        });
    }
    out
}

pub fn is_available(id: ResponseProviderId) -> bool {
    IMPLEMENTED.iter().any(|(known, _)| *known == id)
}

pub fn unavailable_reason(id: ResponseProviderId) -> Option<&'static str> {
    PLANNED
        .iter()
        .find(|(planned, _)| *planned == id)
        .map(|(_, reason)| *reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_implemented_provider_is_available() {
        for (id, _) in IMPLEMENTED {
            assert!(is_available(*id), "{id:?}");
            assert!(unavailable_reason(*id).is_none(), "{id:?}");
        }
    }

    /// O provedor por assinatura ChatGPT aparece, marcado como indisponível e com o motivo
    /// — que é a conclusão do ADR, não uma promessa de "em breve".
    #[test]
    fn chatgpt_subscription_provider_is_listed_as_unavailable_with_a_reason() {
        assert!(!is_available(ResponseProviderId::ChatGptCodexAccount));
        let descriptor = descriptors()
            .into_iter()
            .find(|d| d.id == ResponseProviderId::ChatGptCodexAccount)
            .expect("provedor previsto listado");
        assert!(!descriptor.available);
        let reason = descriptor.unavailable_reason.unwrap();
        assert!(reason.contains("não implementado"), "{reason}");
        assert!(
            reason.contains("adr/chatgpt-codex-subscription-auth"),
            "{reason}"
        );
        assert!(
            !descriptor.capabilities.streaming,
            "não afirmar capacidade de backend não implementado"
        );
    }

    #[test]
    fn local_providers_are_marked_as_local_and_credential_free() {
        for id in [ResponseProviderId::Ollama, ResponseProviderId::LmStudio] {
            let descriptor = descriptors().into_iter().find(|d| d.id == id).unwrap();
            assert!(descriptor.capabilities.local, "{id:?}");
            assert!(!descriptor.capabilities.requires_credentials, "{id:?}");
        }
    }

    #[test]
    fn cloud_providers_declare_that_they_need_credentials() {
        for id in [
            ResponseProviderId::OpenAi,
            ResponseProviderId::DeepSeek,
            ResponseProviderId::Anthropic,
            ResponseProviderId::OpenRouter,
        ] {
            let descriptor = descriptors().into_iter().find(|d| d.id == id).unwrap();
            assert!(!descriptor.capabilities.local, "{id:?}");
            assert!(descriptor.capabilities.requires_credentials, "{id:?}");
        }
    }

    /// Os dois eixos são escolhidos separadamente: transcrever local e gerar na nuvem, ou o
    /// contrário, são combinações igualmente válidas. Este teste existe porque a versão
    /// anterior do código amarrava os dois — Whisper local era pressuposto pelo caminho de
    /// transcrição e Ollama pelo de geração — e qualquer volta a esse acoplamento apareceria
    /// primeiro como um catálogo que conhece o outro.
    #[test]
    fn transcription_and_response_providers_are_chosen_independently() {
        use crate::transcription::fake_provider::{FakeBehavior, FakeTranscriptionProvider};
        use crate::transcription::provider::TranscriptionProviderId;
        use crate::transcription::registry::TranscriptionProviderRegistry;
        use crate::transcription::whisper_local::WhisperLocalTranscriptionProvider;
        use crate::transcription::whisper_provider::WhisperCppProvider;
        use std::sync::Arc;

        let mut transcription = TranscriptionProviderRegistry::new();
        transcription.register(Arc::new(WhisperLocalTranscriptionProvider::new(Arc::new(
            WhisperCppProvider::new(),
        ))));
        let transcription_ids = transcription.registered_ids();
        let response_ids: Vec<_> = descriptors().into_iter().map(|d| d.id).collect();

        // Nenhum dos dois catálogos sabe da existência do outro: os espaços de identidade
        // são disjuntos, então não há como uma escolha excluir a outra.
        for id in &transcription_ids {
            assert!(
                !response_ids.iter().any(|r| r.as_str() == id.as_str()),
                "{id:?} aparece nos dois registries"
            );
        }

        // Transcrição local + geração em nuvem é representável, e o inverso também.
        let whisper = transcription
            .get(TranscriptionProviderId::WhisperLocal)
            .expect("whisper local registrado");
        assert!(whisper.capabilities().local);
        let openai = descriptors()
            .into_iter()
            .find(|d| d.id == ResponseProviderId::OpenAi)
            .unwrap();
        assert!(!openai.capabilities.local);
        let ollama = descriptors()
            .into_iter()
            .find(|d| d.id == ResponseProviderId::Ollama)
            .unwrap();
        assert!(ollama.capabilities.local);

        // Trocar o provider de transcrição não muda nada do lado da geração.
        let before = descriptors().len();
        transcription.register(Arc::new(FakeTranscriptionProvider::new(
            FakeBehavior::Silent,
        )));
        assert_eq!(descriptors().len(), before);
        assert!(is_available(ResponseProviderId::Ollama));
    }

    #[test]
    fn descriptor_ids_are_unique() {
        let all = descriptors();
        let mut ids: Vec<_> = all.iter().map(|d| d.id).collect();
        ids.sort();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "id duplicado no catálogo de provedores");
    }
}
