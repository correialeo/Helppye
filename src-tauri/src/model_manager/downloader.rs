//! Abstração sobre a transferência HTTP em si, para que `manager::ModelManager` seja
//! testável sem depender de rede real nem de baixar arquivos grandes durante os testes.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::model_manager::error::ModelManagerError;

/// `on_progress(downloaded_bytes, total_bytes)` — `total_bytes` é `None` quando o
/// servidor não informa `Content-Length`.
pub type ProgressCallback = Arc<dyn Fn(u64, Option<u64>) + Send + Sync>;

#[async_trait]
pub trait ModelDownloader: Send + Sync {
    /// Baixa `url` para `dest_path`, sobrescrevendo o conteúdo se já existir. Deve
    /// checar `cancel` periodicamente e retornar `Err(ModelManagerError::Cancelled)`
    /// assim que detectado — nunca deixa a chamada pendurada até o fim da transferência.
    async fn download(
        &self,
        url: &str,
        dest_path: &Path,
        cancel: CancellationToken,
        on_progress: ProgressCallback,
    ) -> Result<(), ModelManagerError>;
}

pub struct ReqwestDownloader {
    client: reqwest::Client,
}

impl ReqwestDownloader {
    pub fn new() -> Self {
        ReqwestDownloader {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestDownloader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ModelDownloader for ReqwestDownloader {
    async fn download(
        &self,
        url: &str,
        dest_path: &Path,
        cancel: CancellationToken,
        on_progress: ProgressCallback,
    ) -> Result<(), ModelManagerError> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ModelManagerError::Network(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ModelManagerError::InvalidResponse(format!(
                "HTTP {}",
                response.status()
            )));
        }
        let total = response.content_length();

        let mut file = tokio::fs::File::create(dest_path)
            .await
            .map_err(|e| ModelManagerError::Disk(e.to_string()))?;
        let mut downloaded: u64 = 0;
        let mut stream = response.bytes_stream();

        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return Err(ModelManagerError::Cancelled);
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            file.write_all(&bytes)
                                .await
                                .map_err(|e| ModelManagerError::Disk(e.to_string()))?;
                            downloaded += bytes.len() as u64;
                            on_progress(downloaded, total);
                        }
                        Some(Err(e)) => return Err(ModelManagerError::Network(e.to_string())),
                        None => break,
                    }
                }
            }
        }
        file.flush()
            .await
            .map_err(|e| ModelManagerError::Disk(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    /// Um resultado simulado de download, consumido em ordem a cada chamada de
    /// `download` — permite simular sequências como "falha, depois sucesso" (retry).
    pub enum FakeOutcome {
        /// Escreve `bytes` em pedaços pequenos, chamando `on_progress` a cada pedaço.
        Success(Vec<u8>),
        Http404,
        NetworkError,
        /// Escreve `written_bytes` no destino e então falha, simulando disco cheio
        /// no meio da transferência — o arquivo parcial deve permanecer no disco sob
        /// o nome `.part` até o chamador decidir removê-lo.
        DiskFullAfter(usize),
        /// Como `Success`, mas aguarda `cancel` entre cada pedaço, permitindo que o
        /// teste cancele a operação no meio da transferência.
        SlowSuccess(Vec<u8>),
    }

    pub struct FakeDownloader {
        outcomes: StdMutex<Vec<FakeOutcome>>,
        call_count: AtomicUsize,
    }

    impl FakeDownloader {
        pub fn new(outcomes: Vec<FakeOutcome>) -> Self {
            FakeDownloader {
                outcomes: StdMutex::new(outcomes),
                call_count: AtomicUsize::new(0),
            }
        }

        pub fn call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ModelDownloader for FakeDownloader {
        async fn download(
            &self,
            _url: &str,
            dest_path: &Path,
            cancel: CancellationToken,
            on_progress: ProgressCallback,
        ) -> Result<(), ModelManagerError> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            let outcome = {
                let mut outcomes = self.outcomes.lock().unwrap();
                let clamped = idx.min(outcomes.len() - 1);
                std::mem::replace(&mut outcomes[clamped], FakeOutcome::Http404)
            };

            match outcome {
                FakeOutcome::Success(bytes) => {
                    write_in_chunks(dest_path, &bytes, &on_progress);
                    Ok(())
                }
                FakeOutcome::SlowSuccess(bytes) => {
                    let total = bytes.len() as u64;
                    let mut written = 0u64;
                    let mut file = std::fs::File::create(dest_path)
                        .map_err(|e| ModelManagerError::Disk(e.to_string()))?;
                    use std::io::Write;
                    for chunk in bytes.chunks(4) {
                        if cancel.is_cancelled() {
                            return Err(ModelManagerError::Cancelled);
                        }
                        file.write_all(chunk)
                            .map_err(|e| ModelManagerError::Disk(e.to_string()))?;
                        written += chunk.len() as u64;
                        on_progress(written, Some(total));
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                    if cancel.is_cancelled() {
                        return Err(ModelManagerError::Cancelled);
                    }
                    Ok(())
                }
                FakeOutcome::Http404 => Err(ModelManagerError::InvalidResponse("HTTP 404".into())),
                FakeOutcome::NetworkError => {
                    Err(ModelManagerError::Network("connection reset".into()))
                }
                FakeOutcome::DiskFullAfter(n) => {
                    let bytes = vec![0u8; n];
                    std::fs::write(dest_path, bytes).ok();
                    Err(ModelManagerError::Disk("no space left on device".into()))
                }
            }
        }
    }

    fn write_in_chunks(dest_path: &Path, bytes: &[u8], on_progress: &ProgressCallback) {
        use std::io::Write;
        let total = bytes.len() as u64;
        let mut written = 0u64;
        let mut file = std::fs::File::create(dest_path).expect("create fake download file");
        for chunk in bytes.chunks(4096.max(bytes.len() / 8).max(1)) {
            file.write_all(chunk).expect("write fake download chunk");
            written += chunk.len() as u64;
            on_progress(written, Some(total));
        }
    }
}
