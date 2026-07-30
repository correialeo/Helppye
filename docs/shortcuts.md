# Atalhos de teclado

Implementados em `hooks/useKeyboardShortcuts.ts`, chamado a partir de cada tela que
precisa deles — não um listener global único em `App.tsx` — porque o significado de
"Ctrl/Cmd+D" muda com o contexto (começar vs. encerrar sessão) e "Regenerar" precisa do
turno atual, que só a tela de sessão conhece. Ver `docs/frontend-architecture.md`.

| Atalho | Ação | Onde está ativo |
|---|---|---|
| `Ctrl/Cmd + D` | Começar sessão | `ReadyScreen` |
| `Ctrl/Cmd + D` | Encerrar sessão | `SessionScreen` |
| `Ctrl/Cmd + Enter` | Abrir configurações | `ReadyScreen`, `SessionScreen` |
| `Ctrl/Cmd + Shift + Enter` | Gerar sugestão manualmente ("Regenerar") | `SessionScreen` |

Todos os três são combinações com `mod` (Ctrl no Windows/Linux, ⌘ no macOS via
`components/ui/Kbd.tsx` `modKeyLabel()`), o que é também por que são tratados
globalmente mesmo com um campo de texto focado — nenhum deles colide com digitação
normal, ao contrário de uma tecla solta como `Enter` seria.

Exibidos na interface como chips no estilo tecla (`Kbd`), nunca só como texto — por
exemplo "⌘ D" em `ReadyScreen`, "⌘ ⇧ ⏎" (esmaecido) no rodapé de ações de
`SuggestionPanel` quando uma sugestão concluída está visível.

"Regenerar" (manual) chama `conversation_regenerate_suggestion_command` — ver
`docs/frontend-architecture.md` §Um comando novo no backend, e por quê — para o único
comando novo que esta reformulação adicionou ao lado Rust.
