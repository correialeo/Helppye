//! Harness de benchmark de transcrição: roda o **mesmo** conjunto de áudio contra
//! providers diferentes e produz um relatório comparável.
//!
//! Existe porque a escolha de backend de transcrição hoje é feita por impressão ("pareceu
//! mais rápido", "errou menos"), e impressão não sobrevive a uma troca de modelo. Um
//! provider novo (`docs/transcription-providers.md`) só pode ser recomendado se der para
//! mostrar, sobre o mesmo áudio, quanto ele custa em latência, o que ele erra e onde ele
//! erra.
//!
//! Três decisões de escopo:
//!
//! - **Não usa o app.** O harness fala direto com `TranscriptionProvider` e com
//!   `TranscriptNormalizer`, sem Tauri, sem janela, sem captura de áudio. Medir através da
//!   UI mediria a UI.
//! - **Mede o pipeline até a utterance.** Latência de geração de resposta depende do
//!   provedor de LLM e do prompt, que variam por conta própria; misturar as duas coisas num
//!   número só esconderia qual das duas regrediu. O harness vai do áudio até
//!   `ConversationUtterance`, que é a fronteira em que a transcrição termina seu trabalho.
//! - **Fixtures ficam fora do Git.** Áudio de reunião é conteúdo de terceiro. O manifesto
//!   (texto esperado, vocabulário, idioma) é versionável; os arquivos `.wav` e os resultados
//!   não — ver `benchmarks/README.md` e a entrada correspondente no `.gitignore`.

pub mod fixtures;
pub mod report;
pub mod runner;
pub mod wav;

pub use fixtures::{BenchmarkFixture, FixtureError, FixtureManifest};
pub use report::{write_csv, write_json, ReportError};
pub use runner::{
    run_fixture, run_fixture_with_options, BenchmarkCaseResult, BenchmarkLatencies,
    BenchmarkPacing, BenchmarkRunOptions, CostModel, RunnerError,
};
