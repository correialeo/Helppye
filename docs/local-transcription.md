# Transcrição local: avaliação e escolha do provider

**Escopo:** `src-tauri/src/transcription/` — avaliação do backend de speech-to-text
local a implementar como primeiro (e, por ora, único) `TranscriptionProvider`, feita
**antes** de qualquer código de produção depender dele, conforme exige o item 6 da
especificação da pipeline de transcrição.

**Ambiente de avaliação:** WSL2/Linux, sem GPU, sem hardware de áudio Windows/macOS
real. Toda evidência abaixo vem de uma crate de avaliação isolada em
`/tmp/.../scratchpad/whisper-eval` (fora do pacote real, nunca commitada) — este
documento separa explicitamente o que foi **verificado neste sandbox** do que
**ainda depende de confirmação manual** em Windows real, seguindo a mesma convenção
de `docs/windows-wasapi-loopback.md` (seção 7).

---

## 1. Candidatos considerados

Speech-to-text local, em Rust, sem dependência de nuvem, deixa poucas opções
maduras:

- **`whisper-rs`** — bindings Rust sobre o whisper.cpp (ggerganov/whisper.cpp),
  mantidos ativamente (`tazz4843/whisper-rs`, v0.16.0 no momento desta avaliação).
- **whisper.cpp via FFI manual** — mesma base C++, mas sem bindings prontos:
  significaria escrever e manter o `bindgen`/`build.rs` que `whisper-rs-sys` já
  fornece. Sem vantagem sobre usar `whisper-rs` diretamente; descartado por
  duplicar trabalho sem ganho.
- **Bindings Rust puros para modelos Whisper via `candle` ou `ort` (ONNX Runtime)**
  — existem, mas são bem menos maduros para inferência em tempo real de streaming
  de áudio, e adicionam uma segunda cadeia de toolchain nativa (ONNX Runtime ou o
  próprio `candle`) sem benefício claro sobre whisper.cpp, que é o backend de
  referência do ecossistema Whisper para CPU.
- **Vosk / outros engines não-Whisper** — descartados por qualidade de transcrição
  em português sensivelmente inferior a Whisper nos modelos pequenos/médios, que
  são os viáveis para rodar localmente sem GPU dedicada.

`whisper-rs` foi o único candidato levado a um teste de build real; os demais foram
descartados por inspeção (licença/maturidade/escopo), não teoricamente melhores mas
não testados — isso é declarado aqui para não fabricar uma comparação empírica que
não foi feita.

## 2. Licença

Verificado via `cargo metadata` na crate de avaliação:

- `whisper-rs` e `whisper-rs-sys`: **Unlicense** (equivalente a domínio público).
- Código-fonte do whisper.cpp, vendorizado dentro de `whisper-rs-sys` (não baixado
  em build time — ver seção 3): **MIT**, `Copyright (c) 2023-2024 The ggml authors`,
  confirmado lendo o arquivo `LICENSE` vendorizado diretamente.

Ambas compatíveis com uso comercial/local-first sem obrigação de copyleft. Nenhuma
menção adicional é necessária além da atribuição usual de dependência em
`Cargo.toml`/lockfile.

## 3. Distribuição de modelo — sem download silencioso

Requisito explícito da especificação: nenhum download de modelo sem ação explícita
do usuário.

- O source do **whisper.cpp em si** vem vendorizado dentro do crate
  `whisper-rs-sys` (baixado do crates.io como qualquer dependência Rust, não via
  rede em build time além disso) — `build.rs` foi inspecionado e não contém
  nenhuma lógica de fetch/download de **modelos** (`.bin`, URLs de Hugging Face,
  etc.); as únicas diretivas de rede-adjacentes são `cargo:rustc-link-lib=...`
  para linkagem de backends opcionais (BLAS, CUDA, ...), não download de pesos.
- **Modelos** (arquivos `ggml-*.bin`) são artefatos separados, de responsabilidade
  do usuário/instalador do Helppye — nunca committados no repositório. Isso já é
  consistente com o design existente de `ModelConfig::model_path`
  (`src-tauri/src/transcription/types.rs`), que não tem valor default e trata um
  `model_path` não configurado como `TranscriptionError::NotConfigured`, não como
  gatilho para buscar um modelo automaticamente.
- Este documento **não** prescreve de onde o usuário deve obter o modelo — isso é
  decisão de UX/instalação fora do escopo desta avaliação técnica.

## 4. CPU/GPU

Lido diretamente de `whisper-rs`'s `Cargo.toml`:

```
[features]
default = []
cuda = ["whisper-rs-sys/cuda", "_gpu"]
hipblas = ["whisper-rs-sys/hipblas", "_gpu"]
intel-sycl = ["whisper-rs-sys/intel-sycl", "_gpu"]
metal = ["whisper-rs-sys/metal", "_gpu"]
vulkan = ["whisper-rs-sys/vulkan", "_gpu", "dep:libc"]
coreml = ["whisper-rs-sys/coreml"]
openblas = ["whisper-rs-sys/openblas"]
openmp = ["whisper-rs-sys/openmp"]
log_backend = ["dep:log"]
tracing_backend = ["dep:tracing"]
raw-api = []
test-with-tiny-model = []
```

`default = []` — confirmado empiricamente: a build feita neste sandbox usou features
default e produziu um binário funcionalmente CPU-only, sem nenhuma dependência CUDA/
Metal/Vulkan puxada. Todos os backends de GPU são estritamente opt-in via feature
flag; nenhum é ativado neste momento. Isso mapeia diretamente para
`InferenceDevice::Gpu` em `types.rs`: com as features atuais do Cargo.toml, um
`ModelConfig { device: InferenceDevice::Gpu, .. }` deve falhar alto e explicitamente
(`TranscriptionError`), nunca cair silenciosamente para CPU — nenhum backend de GPU
está compilado.

`tracing_backend` é notável: rotea os logs internos do whisper.cpp (nível C, ex.
`whisper_init_from_file_with_params_no_state: ...`) para a crate `tracing`, que o
projeto já usa (`src-tauri/src/lib.rs`). Sem essa feature, esses logs vão direto
para stdout/stderr fora do controle do `tracing_subscriber`/`EnvFilter` já
configurado — vale habilitá-la quando o provider for implementado.

## 5. Build e footprint — verificado neste sandbox

Toolchain nativa necessária: **CMake** (whisper-rs-sys usa `cmake` para compilar o
whisper.cpp vendorizado) e **libclang** (para `bindgen` gerar os bindings FFI).
Nenhum dos dois estava presente por padrão neste sandbox; ambos foram obtidos sem
`sudo`, via um venv Python (`pip install cmake` → 4.4.0, `pip install libclang` →
18.1.1, Apache 2.0), com `LIBCLANG_PATH` apontado para o `.so` do pacote pip.

Resultado real, target `x86_64-unknown-linux-gnu`:

- `cargo build` (debug): sucesso — `Compiling bindgen v0.72.1` →
  `whisper-rs-sys v0.15.0` → `whisper-rs v0.16.0` → binário de avaliação,
  ~33s de build limpo.
- `cargo build --release`: sucesso, ~31s.
- **Footprint real de linkagem estática**: medido com um `main.rs` que de fato
  chama a API (`WhisperContext::new_with_params`, `create_state`, `FullParams`,
  `state.full`, iteração de segmentos — modelado em cima do próprio
  `examples/basic_use.rs` do crate). Uma primeira medição com uma dependência
  não-utilizada (código morto eliminado pelo linker) deu um número artificialmente
  baixo e foi descartada como não-confiável antes de ser reportada. Com a API
  genuinamente exercida: **binário release de ~1.8 MB (não stripped) / ~1.6 MB
  (stripped)** — esse é o número que deve ser usado como estimativa de impacto no
  tamanho do app; é só o footprint da lib estática (whisper.cpp + ggml, CPU-only),
  **não inclui o arquivo de modelo em si** (não distribuído no binário — seção 3).
- **Comportamento de erro**: `WhisperContext::new_with_params` com um caminho de
  modelo inexistente retorna um `Result::Err` capturável, não pânico/crash — testado
  passando um caminho inválido de propósito. Isso mapeia limpo para
  `TranscriptionError::ModelNotFound`/`ModelLoadFailed`, já existentes em
  `error.rs`, sem necessidade de nenhuma variante nova.

## 6. Build para Windows — não verificado, gap explícito

**Não testado neste sandbox e não pode ser inferido do que foi verificado.** A
técnica usada para validar código Windows-específico em Rust puro (cross-compile
para `x86_64-pc-windows-gnu` sem linker, documentada em `CLAUDE.md`) **não se
estende** a `whisper-rs-sys`: compilar a C++ vendorizada do whisper.cpp para Windows
exige um toolchain CMake+MSVC/MinGW real com headers/libs de Windows, e gerar os
bindings via `bindgen` para esse target exige um `libclang` capaz de parsear
headers de Windows — nada disso foi tentado aqui, e cross-compilar C++ nativo é um
problema categoricamente mais difícil do que cross-compilar o binding puro-Rust já
validado para o WASAPI loopback.

Isso é o maior risco em aberto desta escolha e precisa de confirmação manual em uma
máquina Windows real (idealmente com o mesmo `cargo build` completo, incluindo o
crate de transcrição) antes de considerar o provider pronto para produção. Até essa
confirmação, qualquer afirmação de que "compila em Windows" seria fabricada.

## 7. Decisão

**`whisper-rs` é adotado como o único `TranscriptionProvider` implementado nesta
primeira fase**, via um novo módulo `src-tauri/src/transcription/whisper_provider.rs`
implementando o trait já existente em `provider.rs`. Critérios que pesaram:

- Licença permissiva e compatível (Unlicense + MIT), sem obrigação de copyleft.
- Build real e funcional verificado neste sandbox para Linux CPU, sem `sudo`.
- Nenhum download silencioso de modelo — já alinhado ao design de `ModelConfig`.
- Default `CPU`-only sem custo de GPU não solicitado, com opt-in explícito e
  auditável para backends de GPU se/quando necessário.
- Footprint estático moderado (~1.6 MB stripped), aceitável para um app desktop.
- Erros de carregamento mapeiam limpo para o enum de erro já existente, sem
  necessidade de expandi-lo.
- Feature `tracing_backend` integra diretamente com o `tracing_subscriber` já
  configurado no app.

**Ressalva explícita, não coberta por esta decisão:** dificuldade de build para
Windows (seção 6) permanece um gap real até validação manual em hardware Windows.
A arquitetura por trait (`TranscriptionProvider`) já isola essa decisão — se
`whisper-rs` provar inviável de compilar/linkar em Windows, trocar de provider é
uma nova implementação do trait, não uma reescrita de chamadores.
