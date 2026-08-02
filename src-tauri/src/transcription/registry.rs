//! Registry de providers de transcrição.
//!
//! Existe para que "qual backend está em uso" seja um dado, e não um `match` espalhado por
//! quem precisa construir um provider. Duas propriedades importam:
//!
//! 1. **Um provider previsto mas não implementado não é registrado.** Selecionar
//!    `OpenAiRealtime` hoje devolve `ProviderUnavailable` com o motivo, e não um provider
//!    que aceita áudio e nunca transcreve nada. Um provider que finge funcionar é pior que
//!    um erro: a sessão inteira parece funcionar e não produz uma sugestão sequer.
//! 2. **A UI consulta capacidades declaradas**, via `descriptors()`, em vez de assumir.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;

use crate::transcription::error::TranscriptionError;
use crate::transcription::provider::{
    TranscriptionCapabilities, TranscriptionProvider, TranscriptionProviderId,
};

/// Descrição de um backend para a UI e para diagnósticos. `available == false` significa
/// "o contrato existe, a implementação não" — e `unavailable_reason` diz exatamente o quê.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptionProviderDescriptor {
    pub id: TranscriptionProviderId,
    pub display_name: &'static str,
    pub capabilities: TranscriptionCapabilities,
    pub available: bool,
    pub unavailable_reason: Option<&'static str>,
}

/// Backends cujo contrato está preparado mas cuja implementação **não** existe nesta build.
/// Listados explicitamente para que a UI possa mostrá-los como "ainda não disponível" em
/// vez de escondê-los — e para que a razão seja a mesma no log e na tela.
const PLANNED_PROVIDERS: &[(TranscriptionProviderId, &str)] = &[
    (
        TranscriptionProviderId::OpenAiRealtime,
        "contrato preparado; integração ainda não implementada (ver docs/transcription-providers.md)",
    ),
    (
        TranscriptionProviderId::OpenAiCompatible,
        "contrato preparado; integração ainda não implementada (ver docs/transcription-providers.md)",
    ),
];

#[derive(Default)]
pub struct TranscriptionProviderRegistry {
    providers: BTreeMap<TranscriptionProviderId, Arc<dyn TranscriptionProvider>>,
}

impl TranscriptionProviderRegistry {
    pub fn new() -> Self {
        TranscriptionProviderRegistry::default()
    }

    /// Registrar duas vezes o mesmo id substitui — usado quando o provider local é
    /// reconstruído (troca de modelo), nunca para ter dois backends com o mesmo id.
    pub fn register(&mut self, provider: Arc<dyn TranscriptionProvider>) {
        self.providers.insert(provider.id(), provider);
    }

    pub fn get(
        &self,
        id: TranscriptionProviderId,
    ) -> Result<Arc<dyn TranscriptionProvider>, TranscriptionError> {
        if let Some(provider) = self.providers.get(&id) {
            return Ok(Arc::clone(provider));
        }
        let reason = PLANNED_PROVIDERS
            .iter()
            .find(|(planned, _)| *planned == id)
            .map(|(_, reason)| *reason)
            .unwrap_or("provider desconhecido");
        Err(TranscriptionError::ProviderUnavailable(format!(
            "{}: {reason}",
            id.as_str()
        )))
    }

    pub fn contains(&self, id: TranscriptionProviderId) -> bool {
        self.providers.contains_key(&id)
    }

    pub fn registered_ids(&self) -> Vec<TranscriptionProviderId> {
        self.providers.keys().copied().collect()
    }

    /// Registrados **e** previstos, nessa ordem, para a UI listar tudo com o estado real de
    /// cada um. `Fake` nunca aparece: é infraestrutura de teste.
    pub fn descriptors(&self) -> Vec<TranscriptionProviderDescriptor> {
        let mut out: Vec<TranscriptionProviderDescriptor> = self
            .providers
            .values()
            .filter(|p| p.id() != TranscriptionProviderId::Fake)
            .map(|p| TranscriptionProviderDescriptor {
                id: p.id(),
                display_name: p.id().display_name(),
                capabilities: p.capabilities(),
                available: true,
                unavailable_reason: None,
            })
            .collect();

        for (id, reason) in PLANNED_PROVIDERS {
            if self.providers.contains_key(id) {
                continue;
            }
            out.push(TranscriptionProviderDescriptor {
                id: *id,
                display_name: id.display_name(),
                // Capacidades de um provider não implementado não podem ser afirmadas:
                // `none()` declara o mínimo em vez de prometer streaming/parciais que esta
                // build não entrega.
                capabilities: TranscriptionCapabilities::none(),
                available: false,
                unavailable_reason: Some(reason),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcription::fake_provider::{FakeBehavior, FakeTranscriptionProvider};

    /// `Result::unwrap_err` exigiria `Debug` no lado `Ok`, e `Arc<dyn TranscriptionProvider>`
    /// não tem — nem deveria ter, para não obrigar todo backend a expor estado interno.
    fn expect_error<T>(result: Result<T, TranscriptionError>) -> TranscriptionError {
        match result {
            Ok(_) => panic!("esperava erro, veio um provider"),
            Err(e) => e,
        }
    }

    #[test]
    fn unregistered_planned_provider_fails_with_its_reason() {
        let registry = TranscriptionProviderRegistry::new();
        let err = expect_error(registry.get(TranscriptionProviderId::OpenAiRealtime));
        let message = err.to_string();
        assert!(message.contains("openai_realtime"), "{message}");
        assert!(message.contains("não implementada"), "{message}");
    }

    #[test]
    fn registered_provider_is_returned() {
        let mut registry = TranscriptionProviderRegistry::new();
        registry.register(Arc::new(FakeTranscriptionProvider::new(
            FakeBehavior::Silent,
        )));
        let provider = registry.get(TranscriptionProviderId::Fake).unwrap();
        assert_eq!(provider.id(), TranscriptionProviderId::Fake);
        assert!(registry.contains(TranscriptionProviderId::Fake));
    }

    #[test]
    fn descriptors_expose_planned_providers_as_unavailable_and_hide_the_fake() {
        let mut registry = TranscriptionProviderRegistry::new();
        registry.register(Arc::new(FakeTranscriptionProvider::new(
            FakeBehavior::Silent,
        )));
        let descriptors = registry.descriptors();

        assert!(
            !descriptors
                .iter()
                .any(|d| d.id == TranscriptionProviderId::Fake),
            "o provider de teste não pode aparecer na UI"
        );
        let realtime = descriptors
            .iter()
            .find(|d| d.id == TranscriptionProviderId::OpenAiRealtime)
            .expect("provider previsto listado");
        assert!(!realtime.available);
        assert!(realtime.unavailable_reason.is_some());
        assert!(
            !realtime.capabilities.streaming,
            "não afirmar capacidade de um backend não implementado"
        );
    }

    #[test]
    fn implemented_gemini_descriptor_is_available_and_not_duplicated_as_planned() {
        let mut registry = TranscriptionProviderRegistry::new();
        registry.register(Arc::new(
            FakeTranscriptionProvider::new(FakeBehavior::Silent)
                .with_provider_id(TranscriptionProviderId::GoogleGemini),
        ));

        let descriptors = registry.descriptors();
        let gemini: Vec<_> = descriptors
            .iter()
            .filter(|descriptor| descriptor.id == TranscriptionProviderId::GoogleGemini)
            .collect();
        assert_eq!(gemini.len(), 1);
        assert!(gemini[0].available);
        assert!(gemini[0].capabilities.streaming);
        assert!(gemini[0].capabilities.partial_results);
    }
}
