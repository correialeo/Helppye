//! Orquestra o fluxo de 10 passos do download guiado: resolve diretório → cria pasta
//! `models/` → verifica se já existe e é válido → baixa para `.part` → emite progresso
//! → calcula SHA-256 → compara com o hash esperado → renomeia atomicamente → registra
//! configuração → carrega e valida o modelo. Um arquivo corrompido ou parcial nunca fica
//! sob o nome final: só o rename acontece depois da verificação ter passado.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::model_manager::catalog::{
    find_managed_model, ModelDefinition, ModelLanguageSupport, DEFAULT_MODEL,
};
use crate::model_manager::config_store::{self, TranscriptionModelSelection};
use crate::model_manager::downloader::{ModelDownloader, ProgressCallback};
use crate::model_manager::error::ModelManagerError;
use crate::model_manager::events::ModelDownloadEvent;
use crate::model_manager::state::{ManagedModelsStatus, ModelInstallState, ModelStatus};
use crate::model_manager::verify;
use crate::transcription::segment_transcriber::SegmentTranscriber;
use crate::transcription::types::{InferenceDevice, ModelConfig, TranscriptionLanguage};

/// Intervalo mínimo entre eventos `Progress` emitidos ao frontend — evita inundar o
/// canal IPC em downloads rápidos ou de alta largura de banda (mesmo espírito do "loga
/// a cada 50 descartes" usado em `audio::pipeline`/`transcription::queue`, mas baseado
/// em tempo decorrido, não em contagem de itens).
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(250);

type EventSink = Arc<dyn Fn(ModelDownloadEvent) + Send + Sync>;

pub struct ModelManager {
    models_dir: PathBuf,
    config_path: PathBuf,
    downloader: Arc<dyn ModelDownloader>,
    provider: Arc<dyn SegmentTranscriber>,
    on_event: EventSink,
    state: Mutex<ModelInstallState>,
    active_model_id: Mutex<Option<String>>,
    cancel: Mutex<Option<CancellationToken>>,
}

impl ModelManager {
    pub fn new(
        models_dir: PathBuf,
        config_path: PathBuf,
        downloader: Arc<dyn ModelDownloader>,
        provider: Arc<dyn SegmentTranscriber>,
        on_event: impl Fn(ModelDownloadEvent) + Send + Sync + 'static,
    ) -> Self {
        ModelManager {
            models_dir,
            config_path,
            downloader,
            provider,
            on_event: Arc::new(on_event),
            state: Mutex::new(ModelInstallState::NotInstalled),
            active_model_id: Mutex::new(None),
            cancel: Mutex::new(None),
        }
    }

    fn emit(&self, event: ModelDownloadEvent) {
        (self.on_event)(event);
    }

    async fn set_state(&self, new_state: ModelInstallState) {
        *self.state.lock().await = new_state;
    }

    async fn active_definition(&self) -> ModelDefinition {
        let model_id = self.active_model_id.lock().await;
        model_id
            .as_deref()
            .and_then(find_managed_model)
            .unwrap_or(DEFAULT_MODEL)
    }

    fn status_for(
        definition: ModelDefinition,
        state: ModelInstallState,
        custom_model_path: Option<String>,
    ) -> ModelStatus {
        ModelStatus {
            model_id: definition.id,
            display_name: definition.display_name,
            approximate_size_bytes: definition.approximate_size_bytes,
            state,
            custom_model_path,
            language_support: Some(definition.language_support),
        }
    }

    /// Snapshot combinando o estado atual com os metadados do modelo gerenciado ativo. Se o
    /// estado em memória ainda não foi determinado (`NotInstalled`, isto é, nenhum
    /// download ou verificação rodou nesta sessão), consulta a configuração persistida:
    /// uma seleção de modelo personalizado (feita em uma sessão anterior) reporta seu
    /// próprio estado em vez do modelo padrão; caso contrário, verifica o disco.
    pub async fn status_snapshot(&self) -> Result<ModelStatus, ModelManagerError> {
        let current = self.state.lock().await.clone();
        if current != ModelInstallState::NotInstalled {
            return Ok(Self::status_for(
                self.active_definition().await,
                current,
                None,
            ));
        }

        if let Some(selection) = config_store::load(&self.config_path)? {
            match selection {
                TranscriptionModelSelection::Custom { model_path } => {
                    let state = self.load_persisted_custom_model(&model_path).await;
                    self.set_state(state.clone()).await;
                    return Ok(ModelStatus {
                        model_id: DEFAULT_MODEL.id,
                        display_name: DEFAULT_MODEL.display_name,
                        approximate_size_bytes: DEFAULT_MODEL.approximate_size_bytes,
                        language_support: Some(ModelLanguageSupport::from_model_filename(
                            &model_path,
                        )),
                        state,
                        custom_model_path: Some(model_path),
                    });
                }
                TranscriptionModelSelection::Managed { model_id } => {
                    if let Some(definition) = find_managed_model(&model_id) {
                        *self.active_model_id.lock().await = Some(definition.id.into());
                        let state = self.check_and_load(&definition).await?;
                        return Ok(Self::status_for(definition, state, None));
                    }
                    tracing::warn!(%model_id, "unknown managed transcription model; falling back to default");
                }
            }
        }

        let state = self.check_default_model().await?;
        Ok(Self::status_for(DEFAULT_MODEL, state, None))
    }

    /// Retorna o estado de cada modelo gerenciado sem alterar qual deles está ativo.
    /// Apenas o modelo selecionado é carregado no provider; os demais são verificados
    /// no disco para que a UI consiga distinguir "instalado" de "selecionado".
    pub async fn managed_models_status(&self) -> Result<ManagedModelsStatus, ModelManagerError> {
        let active_model = self.status_snapshot().await?;
        let mut models = Vec::new();

        for definition in crate::model_manager::catalog::MANAGED_MODELS {
            if active_model.custom_model_path.is_none() && active_model.model_id == definition.id {
                models.push(active_model.clone());
                continue;
            }

            let state =
                check_model_at_path(&self.models_dir.join(definition.filename), definition).await?;
            models.push(Self::status_for(*definition, state, None));
        }

        Ok(ManagedModelsStatus {
            active_model,
            models,
        })
    }

    /// Passo 3 isolado: verifica se o modelo padrão já existe e é válido (tamanho +
    /// checksum) e, se estiver, carrega-o de fato no `TranscriptionProvider` — o
    /// arquivo em disco sobrevive a um restart do app, mas o estado em memória do
    /// provider não, então validar só o checksum aqui deixaria `transcribe()` falhando
    /// com "no transcription model configured" mesmo com `state == Ready`.
    pub async fn check_default_model(&self) -> Result<ModelInstallState, ModelManagerError> {
        self.check_and_load(&DEFAULT_MODEL).await
    }

    /// Núcleo de `check_default_model`, parametrizado pela definição do modelo —
    /// separado para ser testável com definições pequenas de teste, já que
    /// `DEFAULT_MODEL` é uma constante de catálogo com um checksum real fixo que não dá
    /// para forjar em teste.
    async fn check_and_load(
        &self,
        definition: &ModelDefinition,
    ) -> Result<ModelInstallState, ModelManagerError> {
        *self.active_model_id.lock().await = Some(definition.id.into());
        self.set_state(ModelInstallState::Checking).await;
        let path = self.models_dir.join(definition.filename);
        let mut state = check_model_at_path(&path, definition).await?;
        if state == ModelInstallState::Ready {
            if let Err(e) = self
                .load_provider(path, definition.display_name.into())
                .await
            {
                state = ModelInstallState::Failed {
                    reason: e.to_string(),
                };
            }
        }
        self.set_state(state.clone()).await;
        Ok(state)
    }

    /// Carrega um caminho de modelo já verificado no `TranscriptionProvider`
    /// configurado — usado tanto para restaurar o modelo padrão quanto um modelo
    /// personalizado persistido ao reabrir o app.
    async fn load_provider(
        &self,
        model_path: PathBuf,
        model_name: String,
    ) -> Result<(), ModelManagerError> {
        let config = ModelConfig {
            model_path,
            model_name,
            language: TranscriptionLanguage::default(),
            device: InferenceDevice::Cpu,
        };
        self.provider
            .load(config)
            .await
            .map_err(|e| ModelManagerError::LoadFailed(e.to_string()))
    }

    /// Restaura uma seleção de modelo personalizado persistida em uma sessão anterior:
    /// o arquivo é revalidado (ainda existe?) e recarregado no provider, já que
    /// `model_name` nunca é persistido junto com `Custom` — deriva um nome de exibição
    /// a partir do nome do arquivo como fallback.
    async fn load_persisted_custom_model(&self, model_path: &str) -> ModelInstallState {
        let path = std::path::Path::new(model_path);
        if !path.is_file() {
            return ModelInstallState::Corrupted {
                reason: "arquivo do modelo personalizado não existe mais".into(),
            };
        }
        let display_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Modelo personalizado".into());
        match self.load_provider(path.to_path_buf(), display_name).await {
            Ok(()) => ModelInstallState::Ready,
            Err(e) => ModelInstallState::Failed {
                reason: e.to_string(),
            },
        }
    }

    pub async fn download_model_by_id(
        &self,
        model_id: Option<&str>,
    ) -> Result<(), ModelManagerError> {
        let definition = match model_id {
            None => DEFAULT_MODEL,
            Some(id) => find_managed_model(id).ok_or_else(|| {
                ModelManagerError::InvalidResponse(format!("unknown transcription model: {id}"))
            })?,
        };
        self.download_model(definition).await
    }

    /// Seleciona um modelo gerenciado que já está instalado, recarrega-o no provider e
    /// persiste a escolha para que ela seja restaurada no próximo início do app.
    pub async fn select_managed_model(&self, model_id: &str) -> Result<(), ModelManagerError> {
        let definition = find_managed_model(model_id).ok_or_else(|| {
            ModelManagerError::InvalidResponse(format!("unknown transcription model: {model_id}"))
        })?;
        let state = self.check_and_load(&definition).await?;
        if state != ModelInstallState::Ready {
            return Err(ModelManagerError::ManagedModelNotInstalled(format!(
                "{} ({state:?})",
                definition.display_name
            )));
        }

        config_store::save(
            &self.config_path,
            &TranscriptionModelSelection::Managed {
                model_id: definition.id.into(),
            },
        )?;
        Ok(())
    }

    pub async fn cancel_download(&self) {
        if let Some(token) = self.cancel.lock().await.as_ref() {
            token.cancel();
        }
    }

    /// Fluxo completo de 10 passos para uma definição de modelo arbitrária — mantido
    /// genérico por definição para ser testável sem depender do arquivo real de ~140 MB
    /// do catálogo (testes usam definições pequenas com um `FakeDownloader`).
    async fn download_model(&self, definition: ModelDefinition) -> Result<(), ModelManagerError> {
        let cancel = CancellationToken::new();
        *self.cancel.lock().await = Some(cancel.clone());

        let result = self.download_model_inner(&definition, cancel).await;

        *self.cancel.lock().await = None;

        match &result {
            Ok(()) => {}
            Err(ModelManagerError::Cancelled) => {
                self.set_state(ModelInstallState::Cancelled).await;
                self.emit(ModelDownloadEvent::Cancelled {
                    model_id: definition.id.into(),
                });
            }
            Err(e) => {
                self.set_state(ModelInstallState::Failed {
                    reason: e.to_string(),
                })
                .await;
                self.emit(ModelDownloadEvent::Failed {
                    model_id: definition.id.into(),
                    error: e.to_string(),
                });
            }
        }
        result
    }

    async fn download_model_inner(
        &self,
        definition: &ModelDefinition,
        cancel: CancellationToken,
    ) -> Result<(), ModelManagerError> {
        *self.active_model_id.lock().await = Some(definition.id.into());
        // 1/2. diretório já resolvido e criado na construção do manager (`paths::resolve_paths`).
        let final_path = self.models_dir.join(definition.filename);
        let part_path = self
            .models_dir
            .join(format!("{}.part", definition.filename));

        // 3. já existe e é válido?
        if verify::verify_file(
            &final_path,
            definition.sha256,
            definition.approximate_size_bytes,
        )
        .is_ok()
        {
            self.load_provider(final_path, definition.display_name.into())
                .await?;
            config_store::save(
                &self.config_path,
                &TranscriptionModelSelection::Managed {
                    model_id: definition.id.into(),
                },
            )?;
            self.set_state(ModelInstallState::Ready).await;
            return Ok(());
        }
        // Um `.part` de uma tentativa anterior nunca deve ser confundido com esta.
        let _ = std::fs::remove_file(&part_path);
        // No Windows, `rename` não substitui um arquivo final existente. Remover
        // somente depois da verificação falhar permite repetir um download corrompido
        // sem deixar o nome final bloquear a instalação nova.
        if final_path.exists() {
            std::fs::remove_file(&final_path)
                .map_err(|e| ModelManagerError::Disk(e.to_string()))?;
        }

        self.set_state(ModelInstallState::Downloading).await;
        self.emit(ModelDownloadEvent::Started {
            model_id: definition.id.into(),
            total_bytes: definition.approximate_size_bytes,
        });

        // 4/5. baixa para `.part`, emitindo progresso (throttled).
        let on_progress = self.make_progress_callback(definition);
        if let Err(e) = self
            .downloader
            .download(definition.download_url, &part_path, cancel, on_progress)
            .await
        {
            let _ = std::fs::remove_file(&part_path);
            return Err(e);
        }

        // 6/7. verificação de integridade.
        self.set_state(ModelInstallState::Verifying).await;
        self.emit(ModelDownloadEvent::Verifying {
            model_id: definition.id.into(),
        });

        let verify_path = part_path.clone();
        let sha256 = definition.sha256;
        let expected_size = definition.approximate_size_bytes;
        let verify_result = tokio::task::spawn_blocking(move || {
            verify::verify_file(&verify_path, sha256, expected_size)
        })
        .await
        .map_err(|e| ModelManagerError::Disk(format!("verification task panicked: {e}")))?;

        if let Err(e) = verify_result {
            let _ = std::fs::remove_file(&part_path);
            self.set_state(ModelInstallState::Corrupted {
                reason: e.to_string(),
            })
            .await;
            return Err(e);
        }

        // 8. rename atômico — só acontece depois de verificado, então o nome final
        // nunca aponta para um arquivo corrompido ou parcial.
        self.set_state(ModelInstallState::Installing).await;
        std::fs::rename(&part_path, &final_path)
            .map_err(|e| ModelManagerError::Disk(e.to_string()))?;

        // 10. carrega e valida o modelo de fato (não só o arquivo em disco).
        self.load_provider(final_path.clone(), definition.display_name.into())
            .await?;

        // Só torna a seleção persistente depois que o provider confirmou que consegue
        // abrir o arquivo. Um modelo incompatível não deve substituir o último válido.
        config_store::save(
            &self.config_path,
            &TranscriptionModelSelection::Managed {
                model_id: definition.id.into(),
            },
        )?;

        self.set_state(ModelInstallState::Ready).await;
        self.emit(ModelDownloadEvent::Completed {
            model_id: definition.id.into(),
            path: final_path.display().to_string(),
        });
        Ok(())
    }

    fn make_progress_callback(&self, definition: &ModelDefinition) -> ProgressCallback {
        let on_event = self.on_event.clone();
        let model_id = definition.id.to_string();
        let fallback_total = definition.approximate_size_bytes;
        let start = Instant::now();
        let last_emit = std::sync::Mutex::new(Instant::now() - PROGRESS_EMIT_INTERVAL);

        Arc::new(move |downloaded: u64, total: Option<u64>| {
            let total = total.unwrap_or(fallback_total);
            {
                let mut guard = last_emit.lock().expect("progress throttle mutex poisoned");
                let now = Instant::now();
                let should_emit =
                    downloaded >= total || now.duration_since(*guard) >= PROGRESS_EMIT_INTERVAL;
                if !should_emit {
                    return;
                }
                *guard = now;
            }
            let elapsed = start.elapsed().as_secs_f64();
            let bytes_per_second = if elapsed > 0.0 {
                downloaded as f64 / elapsed
            } else {
                0.0
            };
            on_event(ModelDownloadEvent::Progress {
                model_id: model_id.clone(),
                downloaded_bytes: downloaded,
                total_bytes: total,
                bytes_per_second,
            });
        })
    }

    /// Fluxo de configuração avançada: valida `path` carregando-o de fato através do
    /// `TranscriptionProvider` configurado antes de persistir a seleção — nunca salva
    /// um caminho não validado.
    pub async fn select_custom_model(
        &self,
        path: PathBuf,
        model_name: String,
    ) -> Result<(), ModelManagerError> {
        if !path.is_file() {
            return Err(ModelManagerError::InvalidCustomModel(format!(
                "file not found: {}",
                path.display()
            )));
        }
        let config = ModelConfig {
            model_path: path.clone(),
            model_name,
            language: TranscriptionLanguage::default(),
            device: InferenceDevice::Cpu,
        };
        self.provider
            .load(config)
            .await
            .map_err(|e| ModelManagerError::InvalidCustomModel(e.to_string()))?;

        config_store::save(
            &self.config_path,
            &TranscriptionModelSelection::Custom {
                model_path: path.display().to_string(),
            },
        )?;
        self.set_state(ModelInstallState::Ready).await;
        Ok(())
    }
}

async fn check_model_at_path(
    path: &std::path::Path,
    definition: &ModelDefinition,
) -> Result<ModelInstallState, ModelManagerError> {
    if !path.is_file() {
        return Ok(ModelInstallState::NotInstalled);
    }
    let path = path.to_path_buf();
    let sha256 = definition.sha256;
    let expected_size = definition.approximate_size_bytes;
    let result =
        tokio::task::spawn_blocking(move || verify::verify_file(&path, sha256, expected_size))
            .await
            .map_err(|e| ModelManagerError::Disk(format!("verification task panicked: {e}")))?;
    match result {
        Ok(()) => Ok(ModelInstallState::Ready),
        Err(e) => Ok(ModelInstallState::Corrupted {
            reason: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_manager::catalog::ModelLanguageSupport;
    use crate::model_manager::downloader::fake::{FakeDownloader, FakeOutcome};
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "helppye-manager-test-{label}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    struct FakeProvider {
        fail: bool,
        load_calls: StdMutex<Vec<PathBuf>>,
    }

    impl FakeProvider {
        fn new(fail: bool) -> Self {
            FakeProvider {
                fail,
                load_calls: StdMutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl SegmentTranscriber for FakeProvider {
        async fn load(
            &self,
            config: ModelConfig,
        ) -> Result<(), crate::transcription::error::TranscriptionError> {
            self.load_calls.lock().unwrap().push(config.model_path);
            if self.fail {
                return Err(
                    crate::transcription::error::TranscriptionError::ModelLoadFailed(
                        "fake load failure".into(),
                    ),
                );
            }
            Ok(())
        }

        async fn transcribe(
            &self,
            _segment: crate::audio::segment::AudioSegment,
        ) -> Result<
            crate::transcription::types::Transcript,
            crate::transcription::error::TranscriptionError,
        > {
            unimplemented!("not exercised by model_manager tests")
        }

        fn provider_name(&self) -> &'static str {
            "fake"
        }
    }

    /// Gera uma definição de modelo pequena (poucos KB) com um checksum real,
    /// computado sobre os próprios bytes de teste — nunca inventado, e nunca
    /// dependente do arquivo oficial de ~140 MB usado em produção.
    fn test_definition(id: &'static str, filename: &'static str) -> (ModelDefinition, Vec<u8>) {
        let bytes: Vec<u8> = (0..2048u32).map(|n| (n % 251) as u8).collect();
        let tmp_path = std::env::temp_dir().join(format!(
            "helppye-hash-scratch-{id}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&tmp_path, &bytes).unwrap();
        let hash = verify::compute_sha256(&tmp_path).unwrap();
        std::fs::remove_file(&tmp_path).ok();

        let definition = ModelDefinition {
            id,
            display_name: "Test Model",
            filename,
            download_url: "https://example.invalid/model.bin",
            sha256: Box::leak(hash.into_boxed_str()),
            approximate_size_bytes: bytes.len() as u64,
            language_support: ModelLanguageSupport::Multilingual,
        };
        (definition, bytes)
    }

    fn manager_with(
        dir: &std::path::Path,
        downloader: Arc<dyn ModelDownloader>,
        provider: Arc<dyn SegmentTranscriber>,
    ) -> ModelManager {
        manager_with_events(
            dir,
            downloader,
            provider,
            Arc::new(StdMutex::new(Vec::new())),
        )
        .0
    }

    fn manager_with_events(
        dir: &std::path::Path,
        downloader: Arc<dyn ModelDownloader>,
        provider: Arc<dyn SegmentTranscriber>,
        events: Arc<StdMutex<Vec<ModelDownloadEvent>>>,
    ) -> (ModelManager, Arc<StdMutex<Vec<ModelDownloadEvent>>>) {
        std::fs::create_dir_all(dir.join("models")).unwrap();
        let events_cb = events.clone();
        let manager = ModelManager::new(
            dir.join("models"),
            dir.join("transcription.json"),
            downloader,
            provider,
            move |event| events_cb.lock().unwrap().push(event),
        );
        (manager, events)
    }

    #[tokio::test]
    async fn check_model_at_path_reports_not_installed_when_missing() {
        let tmp = TempDir::new("absent");
        let (definition, _bytes) = test_definition("t1", "model.bin");
        let missing_path = tmp.path().join(definition.filename);

        let state = super::check_model_at_path(&missing_path, &definition)
            .await
            .unwrap();

        assert_eq!(state, ModelInstallState::NotInstalled);
    }

    #[tokio::test]
    async fn check_model_at_path_reports_ready_for_a_valid_file() {
        let tmp = TempDir::new("valid");
        let (definition, bytes) = test_definition("t2", "model.bin");
        let path = tmp.path().join(definition.filename);
        std::fs::write(&path, &bytes).unwrap();

        let state = super::check_model_at_path(&path, &definition)
            .await
            .unwrap();

        assert_eq!(state, ModelInstallState::Ready);
    }

    #[tokio::test]
    async fn check_model_at_path_reports_corrupted_for_checksum_mismatch() {
        let tmp = TempDir::new("corrupted");
        let (definition, bytes) = test_definition("t3", "model.bin");
        let path = tmp.path().join(definition.filename);
        let mut tampered = bytes;
        tampered[0] ^= 0xFF;
        std::fs::write(&path, &tampered).unwrap();

        let state = super::check_model_at_path(&path, &definition)
            .await
            .unwrap();

        assert!(matches!(state, ModelInstallState::Corrupted { .. }));
    }

    #[tokio::test]
    async fn a_stray_part_file_is_never_considered_installed() {
        let tmp = TempDir::new("partial");
        let (definition, bytes) = test_definition("t4", "model.bin");
        // Only the `.part` file exists — the final filename does not.
        let part_path = tmp.path().join(format!("{}.part", definition.filename));
        std::fs::write(&part_path, &bytes).unwrap();
        let final_path = tmp.path().join(definition.filename);

        let state = super::check_model_at_path(&final_path, &definition)
            .await
            .unwrap();

        assert_eq!(state, ModelInstallState::NotInstalled);
    }

    #[tokio::test]
    async fn successful_download_emits_progress_events_and_reaches_ready() {
        let tmp = TempDir::new("progress");
        let (definition, bytes) = test_definition("t5", "model.bin");
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::Success(bytes)]));
        let provider = Arc::new(FakeProvider::new(false));
        let (manager, events) = manager_with_events(
            tmp.path(),
            downloader,
            provider,
            Arc::new(StdMutex::new(Vec::new())),
        );

        manager.download_model(definition).await.unwrap();

        // Tira o snapshot antes de travar `events`: o `StdMutex` não é ciente de async, e
        // segurá-lo através de um `.await` trava o executor se a chamada ceder.
        let state = manager.status_snapshot().await.unwrap().state;

        let events = events.lock().unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, ModelDownloadEvent::Started { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, ModelDownloadEvent::Progress { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, ModelDownloadEvent::Completed { .. })));
        assert_eq!(state, ModelInstallState::Ready);
    }

    #[tokio::test]
    async fn download_loads_the_model_through_the_transcription_provider() {
        let tmp = TempDir::new("load-after-download");
        let (definition, bytes) = test_definition("t6", "model.bin");
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::Success(bytes)]));
        let provider = Arc::new(FakeProvider::new(false));
        let manager = manager_with(tmp.path(), downloader, provider.clone());
        let filename = definition.filename;

        manager.download_model(definition).await.unwrap();

        let calls = provider.load_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], tmp.path().join("models").join(filename));
    }

    #[tokio::test]
    async fn cancelling_mid_download_leaves_no_final_or_partial_file() {
        let tmp = TempDir::new("cancel");
        let (definition, bytes) = test_definition("t7", "model.bin");
        let filename = definition.filename;
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::SlowSuccess(bytes)]));
        let provider = Arc::new(FakeProvider::new(false));
        let manager = Arc::new(manager_with(tmp.path(), downloader, provider));

        let manager_bg = manager.clone();
        let handle = tokio::spawn(async move { manager_bg.download_model(definition).await });

        tokio::time::sleep(Duration::from_millis(30)).await;
        manager.cancel_download().await;

        let result = handle.await.unwrap();
        assert_eq!(result, Err(ModelManagerError::Cancelled));
        assert_eq!(
            manager.status_snapshot().await.unwrap().state,
            ModelInstallState::Cancelled
        );

        let final_path = tmp.path().join("models").join(filename);
        let part_path = tmp.path().join("models").join(format!("{filename}.part"));
        assert!(!final_path.exists());
        assert!(!part_path.exists());
    }

    #[tokio::test]
    async fn http_error_surfaces_as_failed_and_leaves_no_files_behind() {
        let tmp = TempDir::new("http-error");
        let (definition, _bytes) = test_definition("t8", "model.bin");
        let filename = definition.filename;
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::Http404]));
        let provider = Arc::new(FakeProvider::new(false));
        let manager = manager_with(tmp.path(), downloader, provider);

        let result = manager.download_model(definition).await;

        assert!(matches!(result, Err(ModelManagerError::InvalidResponse(_))));
        assert!(matches!(
            manager.status_snapshot().await.unwrap().state,
            ModelInstallState::Failed { .. }
        ));
        let part_path = tmp.path().join("models").join(format!("{filename}.part"));
        assert!(!part_path.exists());
    }

    /// Queda de rede é o modo de falha mais comum num download de ~150 MB, e é diferente
    /// de um 404: o arquivo existe, a conexão é que caiu. O contrato é o mesmo — falha
    /// tipada como `Network`, nada de `.part` sobrando para o retry tropeçar.
    #[tokio::test]
    async fn network_failure_surfaces_as_failed_and_leaves_no_files_behind() {
        let tmp = TempDir::new("network-error");
        let (definition, _bytes) = test_definition("t8b", "model.bin");
        let filename = definition.filename;
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::NetworkError]));
        let provider = Arc::new(FakeProvider::new(false));
        let manager = manager_with(tmp.path(), downloader, provider);

        let result = manager.download_model(definition).await;

        assert!(matches!(result, Err(ModelManagerError::Network(_))));
        assert!(matches!(
            manager.status_snapshot().await.unwrap().state,
            ModelInstallState::Failed { .. }
        ));
        let part_path = tmp.path().join("models").join(format!("{filename}.part"));
        assert!(!part_path.exists());
    }

    #[tokio::test]
    async fn simulated_disk_full_fails_and_removes_the_partial_file() {
        let tmp = TempDir::new("disk-full");
        let (definition, _bytes) = test_definition("t9", "model.bin");
        let filename = definition.filename;
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::DiskFullAfter(512)]));
        let provider = Arc::new(FakeProvider::new(false));
        let manager = manager_with(tmp.path(), downloader, provider);

        let result = manager.download_model(definition).await;

        assert!(matches!(result, Err(ModelManagerError::Disk(_))));
        let final_path = tmp.path().join("models").join(filename);
        let part_path = tmp.path().join("models").join(format!("{filename}.part"));
        assert!(!final_path.exists());
        assert!(!part_path.exists());
    }

    #[tokio::test]
    async fn retrying_after_a_failure_succeeds_on_the_second_attempt() {
        let tmp = TempDir::new("retry");
        let (definition, bytes) = test_definition("t10", "model.bin");
        let downloader = Arc::new(FakeDownloader::new(vec![
            FakeOutcome::Http404,
            FakeOutcome::Success(bytes),
        ]));
        let provider = Arc::new(FakeProvider::new(false));
        let manager = manager_with(tmp.path(), downloader.clone(), provider);

        let first = manager.download_model(definition).await;
        assert!(first.is_err());

        let second = manager.download_model(definition).await;
        assert!(second.is_ok());
        assert_eq!(downloader.call_count(), 2);
        assert_eq!(
            manager.status_snapshot().await.unwrap().state,
            ModelInstallState::Ready
        );
    }

    #[tokio::test]
    async fn retry_replaces_an_invalid_final_file_before_renaming_on_windows() {
        let tmp = TempDir::new("retry-invalid-final");
        let (definition, bytes) = test_definition("t10b", "model.bin");
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::Success(
            bytes.clone(),
        )]));
        let provider = Arc::new(FakeProvider::new(false));
        let manager = manager_with(tmp.path(), downloader, provider);
        let final_path = tmp.path().join("models").join(definition.filename);
        std::fs::write(&final_path, b"corrupted old model").unwrap();

        manager.download_model(definition).await.unwrap();

        assert_eq!(std::fs::read(final_path).unwrap(), bytes);
    }

    #[tokio::test]
    async fn selecting_a_nonexistent_custom_model_is_rejected() {
        let tmp = TempDir::new("custom-invalid");
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::Http404]));
        let provider = Arc::new(FakeProvider::new(false));
        let manager = manager_with(tmp.path(), downloader, provider.clone());

        let result = manager
            .select_custom_model(tmp.path().join("does-not-exist.bin"), "custom".into())
            .await;

        assert!(matches!(
            result,
            Err(ModelManagerError::InvalidCustomModel(_))
        ));
        assert!(provider.load_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn selecting_a_valid_custom_model_loads_it_and_persists_the_selection() {
        let tmp = TempDir::new("custom-valid");
        let custom_model_path = tmp.path().join("my-custom-model.bin");
        std::fs::write(&custom_model_path, b"not a real ggml file, but present").unwrap();
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::Http404]));
        let provider = Arc::new(FakeProvider::new(false));
        let manager = manager_with(tmp.path(), downloader, provider.clone());

        let result = manager
            .select_custom_model(custom_model_path.clone(), "my custom model".into())
            .await;

        assert!(result.is_ok());
        assert_eq!(provider.load_calls.lock().unwrap()[0], custom_model_path);
        let persisted = config_store::load(&tmp.path().join("transcription.json"))
            .unwrap()
            .unwrap();
        assert_eq!(
            persisted,
            TranscriptionModelSelection::Custom {
                model_path: custom_model_path.display().to_string()
            }
        );
    }

    // Reproduz o bug relatado: após reiniciar o app (novo `ModelManager` + novo
    // `TranscriptionProvider`, já que ambos são recriados a cada processo), o modelo
    // padrão já baixado numa sessão anterior precisa ser recarregado de fato no
    // provider, não só ter seu checksum revalidado em disco.
    #[tokio::test]
    async fn restarting_the_app_reloads_the_already_downloaded_default_model_into_a_fresh_provider()
    {
        let tmp = TempDir::new("restart-default");
        let (definition, bytes) = test_definition("t11", "model.bin");
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::Success(bytes)]));
        let first_provider = Arc::new(FakeProvider::new(false));
        let manager = manager_with(tmp.path(), downloader.clone(), first_provider.clone());
        manager.download_model(definition).await.unwrap();
        assert_eq!(first_provider.load_calls.lock().unwrap().len(), 1);

        // Simula o restart: novo manager, novo provider "vazio" — mas o mesmo arquivo
        // em disco de antes (o `TempDir` persiste entre os dois managers). Usa
        // `check_and_load` diretamente (em vez de `status_snapshot`, que sempre checa
        // contra `DEFAULT_MODEL`, uma constante de catálogo com checksum real fixo que
        // não dá para forjar com a definição pequena de teste).
        let fresh_provider = Arc::new(FakeProvider::new(false));
        let fresh_manager = manager_with(tmp.path(), downloader, fresh_provider.clone());

        let state = fresh_manager.check_and_load(&definition).await.unwrap();

        assert_eq!(state, ModelInstallState::Ready);
        assert_eq!(fresh_provider.load_calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn restarting_the_app_reloads_a_persisted_custom_model_into_a_fresh_provider() {
        let tmp = TempDir::new("restart-custom");
        let custom_model_path = tmp.path().join("my-custom-model.bin");
        std::fs::write(&custom_model_path, b"not a real ggml file, but present").unwrap();
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::Http404]));
        let first_provider = Arc::new(FakeProvider::new(false));
        let manager = manager_with(tmp.path(), downloader.clone(), first_provider.clone());
        manager
            .select_custom_model(custom_model_path.clone(), "my custom model".into())
            .await
            .unwrap();

        let fresh_provider = Arc::new(FakeProvider::new(false));
        let fresh_manager = manager_with(tmp.path(), downloader, fresh_provider.clone());

        let status = fresh_manager.status_snapshot().await.unwrap();

        assert_eq!(status.state, ModelInstallState::Ready);
        assert_eq!(fresh_provider.load_calls.lock().unwrap().len(), 1);
        assert_eq!(
            fresh_provider.load_calls.lock().unwrap()[0],
            custom_model_path
        );
    }

    #[tokio::test]
    async fn restart_surfaces_a_failed_state_when_the_fresh_provider_load_fails() {
        let tmp = TempDir::new("restart-load-failure");
        let (definition, bytes) = test_definition("t12", "model.bin");
        let downloader = Arc::new(FakeDownloader::new(vec![FakeOutcome::Success(bytes)]));
        let first_provider = Arc::new(FakeProvider::new(false));
        let manager = manager_with(tmp.path(), downloader.clone(), first_provider.clone());
        manager.download_model(definition).await.unwrap();

        let failing_provider = Arc::new(FakeProvider::new(true));
        let fresh_manager = manager_with(tmp.path(), downloader, failing_provider);

        let state = fresh_manager.check_and_load(&definition).await.unwrap();

        assert!(matches!(state, ModelInstallState::Failed { .. }));
    }
}
