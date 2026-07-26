# Componentes de terceiros adaptados

Este documento registra todo componente copiado/adaptado de outro projeto para dentro do Helppye, conforme exigido pela auditoria em `docs/meetily-audio-audit.md`. Cada entrada deve conter: caminho original, hash do commit de origem, novo caminho, tipo de alteração, licença e data.

**Estado atual: nenhum código foi copiado ou adaptado do Meetily ainda.** A auditoria (`docs/meetily-audio-audit.md`, seção 7) identificou um único candidato à Estratégia B — adaptação com atribuição — e ele só será efetivamente adaptado quando a implementação da plataforma macOS começar (fora do escopo desta primeira execução, que não pode ser testada em macOS real neste ambiente WSL2). A entrada abaixo é reservada/planejada, não efetivada.

## Planejado (ainda não implementado)

| Campo | Valor |
|---|---|
| Caminho original | `meetily/frontend/src-tauri/src/audio/capture/core_audio.rs` |
| Commit de origem | `0281737d87d26352fb0adc78c8c0975f691b23d1` (branch `fix/audio-mixing`, 2026-06-05 19:22:04 +0530) |
| Novo caminho (planejado) | `src-tauri/src/audio/platform/macos.rs` (Helppye) |
| Tipo de alteração (planejada) | Adaptação — isolar da mixagem/gravação do Meetily, reconstruir a saída em torno de `tokio::sync::mpsc::channel` limitado e do trait `AudioCaptureProvider` do Helppye, preservando a lógica central de uso da API Core Audio Process Tap via `cidre` |
| Licença de origem | MIT, Copyright (c) 2024 Zackriya Solutions |
| Data de adaptação | Não realizada ainda — pendente de implementação da Fase 2/plataforma macOS e de teste em hardware macOS 14.4+ real |
| Notas | Ver seção 4 e 7 de `docs/meetily-audio-audit.md` para a justificativa completa da escolha desta exceção pontual à Estratégia C (referência apenas) adotada para o restante do módulo de áudio do Meetily |

## Convenção para novas entradas

Ao adaptar efetivamente qualquer trecho do Meetily (ou de outro projeto) no futuro, adicionar uma nova linha/tabela aqui **antes** de mesclar o código, e incluir no topo do arquivo adaptado um comentário no formato:

```
// Adaptado de meetily (MIT, Copyright (c) 2024 Zackriya Solutions)
// Origem: frontend/src-tauri/src/audio/capture/core_audio.rs @ 0281737d87d26352fb0adc78c8c0975f691b23d1
// Ver docs/third-party-components.md
```
