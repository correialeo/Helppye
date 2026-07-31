# Atalhos

| Atalho | Acao | Onde esta ativo |
| --- | --- | --- |
| `Ctrl + D` (Windows/Linux) / `Cmd + D` (macOS) | Comecar sessao se nao houver sessao ativa; encerrar sessao se houver | Global, mesmo com Helppye sem foco ou minimizado |
| `Ctrl/Cmd + Enter` | Abrir configuracoes | `ReadyScreen`, `SessionScreen` com foco |
| `Ctrl/Cmd + Shift + Enter` | Gerar sugestao manualmente ("Regenerar") | `SessionScreen` com foco |

## Limitacoes

O atalho global usa o mecanismo nativo exposto por `tauri-plugin-global-shortcut`.
Se o sistema operacional, gerenciador de janelas, politica corporativa ou outro
aplicativo ja tiver reservado a combinacao, o registro pode falhar. Nesse caso o
Helppye registra o erro em log e os atalhos locais continuam funcionando quando a
janela do Helppye estiver com foco.
