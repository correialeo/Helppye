# ADR — Usar uma assinatura ChatGPT Plus/Pro no Helppye via autenticação do Codex

- **Status:** aceito
- **Data:** 2026-07-31
- **Conclusão:** **Unsupported** para o que foi pedido (Helppye falar direto com a
  autenticação da OpenAI usada pelo Codex e consumir a assinatura do usuário).
  O único caminho oficialmente documentado — hospedar o **Codex App Server** como processo
  local — existe, é real e está descrito abaixo, mas fica classificado como
  **Needs official clarification** para o caso de uso do Helppye (conteúdo de reunião num
  agente de código). Nenhum provider foi implementado nesta execução.

---

## 1. Contexto

Foi observado, em outro aplicativo (Perssua), um fluxo de login em que o navegador abre a
autenticação da OpenAI/Codex e volta para um callback loopback na porta 1455 com
`code`, `scope=openid profile email offline_access` e `state`. Isso sugere OAuth
Authorization Code com refresh token, e levantou a pergunta: o Helppye pode oferecer
"conecte sua conta ChatGPT Plus/Pro" e gerar sugestões de resposta usando a assinatura,
em vez de exigir uma API key paga à parte?

A investigação usou **apenas** fontes oficiais:

- documentação oficial da OpenAI (`developers.openai.com/codex/*`, hoje redirecionado
  para `learn.chatgpt.com/docs/*`; políticas em `openai.com/policies/*`);
- o código-fonte oficial do Codex (`github.com/openai/codex`, Apache-2.0), lido pela API
  pública do GitHub.

Nada foi obtido do Perssua. Nenhum tráfego, binário, token, client ID ou client secret
daquele aplicativo foi inspecionado, capturado ou reutilizado. O `code` que aparecia na
URL de callback do exemplo é uma credencial real de terceiro: não foi usado, não foi
registrado em log e não aparece em nenhum teste deste repositório.

## 2. Respostas às 14 perguntas

### 1. O Codex oferece autenticação oficial por conta ChatGPT?

**Sim.** É o caminho padrão e recomendado do produto. A documentação oficial descreve
`codex login` abrindo o navegador ("Sign in with ChatGPT"), o retorno via servidor local,
o fluxo alternativo de device code (`codex login --device-auth`) e o cache de credenciais.
Isso é autenticação **do Codex**, para as superfícies do Codex (CLI, extensão de IDE,
app), não um provedor de identidade genérico para aplicativos de terceiros.

### 2. O acesso da assinatura é explicitamente permitido para aplicações externas?

**Não há permissão explícita para o que o Helppye faria.** O que existe:

- A documentação de autenticação do Codex é inteiramente de primeira parte — CLI,
  extensão, app. Não há concessão para um aplicativo externo conduzir o fluxo por conta
  própria, nem endpoint de registro de cliente OAuth público para `auth.openai.com`.
- Existe um "Sign in with ChatGPT" para parceiros, em beta e com parceiros nomeados, mas
  ele compartilha **nome, e-mail e foto** — é identidade, não cota de inferência da
  assinatura.
- Os termos da OpenAI proíbem compartilhar credenciais de conta e responsabilizam o
  titular por toda atividade sob ela.

O único caminho documentado em que um produto de terceiro usa a assinatura do usuário é
**não tocar na credencial**: hospedar o Codex (ver pergunta 3) e deixar que ele seja o
dono da autenticação.

### 3. O Codex App Server é o caminho recomendado para integração?

**Sim, esse é o caminho oficial.** A documentação do Codex App Server diz, textualmente,
que ele é a interface que o próprio Codex usa para clientes ricos (a extensão do VS Code)
e que serve para "uma integração profunda dentro do seu próprio produto": autenticação,
histórico de conversas, aprovações e eventos do agente em streaming. É um processo
stateful de vida longa que expõe JSON-RPC 2.0 (transporte padrão stdio).

Ele expõe explicitamente uma superfície de conta:
`account/read`, `account/login/start` (com `type: "chatgpt"` ou `"chatgptDeviceCode"`),
`account/login/cancel`, `account/logout`, `account/rateLimits/read` e as notificações
`account/updated` / `account/login/completed`. O modo `chatgpt` é descrito como
recomendado, com a frase decisiva para este ADR: **"Codex owns the ChatGPT OAuth flow and
refresh tokens"** — o cliente pede o login e recebe eventos; ele nunca vê `code`,
`client_id`, access token ou refresh token.

A documentação também pede que integrações se identifiquem via `clientInfo`, e que quem
estiver construindo uma integração nova voltada a uso corporativo entre em contato com a
OpenAI para entrar numa lista de clientes conhecidos.

### 4. É possível embutir ou executar o App Server como processo local?

**Sim.** `codex app-server` roda como processo local falando JSON-RPC por stdio (também há
socket unix; o transporte WebSocket é marcado como experimental/não suportado e não deve
ser usado em produção). Um aplicativo Tauri consegue, tecnicamente, iniciar esse processo
e conversar com ele.

Mas isso **não** é "embutir": o binário do Codex é distribuído pela OpenAI e teria que
estar instalado e logado na máquina do usuário. O Helppye passaria a depender de um
executável externo, da versão dele (o schema JSON-RPC é gerado por versão, `codex
app-server generate-ts`) e do estado de login dele. Redistribuir o binário dentro do
instalador do Helppye é uma decisão de licenciamento/marca separada, que este ADR não
autoriza.

### 5. O App Server fornece geração textual genérica ou é orientado a tarefas de código?

**Orientado a tarefas de código.** As primitivas são `thread` / `turn` / `item` de um
**agente de codificação**: `turn/start` aceita `cwd`, política de sandbox, perfis de
permissão e revisor de aprovações; há `command/exec`, `process/spawn`,
`thread/shellCommand`, integração com MCP, skills e review de código. Não existe um
endpoint de "complete este texto".

Dá para pedir texto livre a um agente? Dá. Mas seria usar um agente com sandbox de
sistema de arquivos e execução de comandos como se fosse uma API de chat — pagando a
latência e a superfície de risco de tudo isso para obter três frases de sugestão de
resposta. É o oposto do requisito de latência do Helppye (silêncio → token visível).

### 6. Quais modelos e limites ficam disponíveis?

Os modelos são os que o Codex oferece ao plano do usuário, definidos pelo backend, não
escolhidos livremente pelo integrador. Os limites são os limites de taxa do plano ChatGPT,
legíveis via `account/rateLimits/read` (inclui limite mensal de crédito efetivo, se
atingiu controle de gasto, e créditos de reset ganhos) e atualizados por
`account/rateLimits/updated`. Ou seja: quota compartilhada com o uso normal de Codex da
pessoa. Uma reunião de uma hora gerando sugestões consumiria o mesmo saldo que ela usa
para programar.

### 7. Como refresh, logout e revogação funcionam?

No caminho oficial, **nada disso é responsabilidade do Helppye**: "Codex persists tokens
to disk and refreshes them automatically". A documentação de autenticação confirma que o
Codex renova tokens automaticamente antes de expirarem. Logout é `account/logout` (ou
`codex logout`), que remove as credenciais salvas. O código oficial mantém os endpoints de
`token` e `revoke` sob `https://auth.openai.com/oauth/*`, chamados pelo próprio Codex.

### 8. Onde os tokens oficiais são armazenados?

Em `~/.codex/auth.json` (texto puro) **ou** no cofre de credenciais do sistema
operacional, conforme a opção `cli_auth_credentials_store` (`file`, `keyring`, `auto`). A
documentação repete o aviso de tratar `auth.json` como senha. CLI e extensão compartilham
o mesmo cache.

### 9. O fluxo utiliza PKCE?

**Sim.** A documentação pública não descreve o protocolo, mas o código oficial de login do
Codex usa PKCE explicitamente (o servidor de callback gera os códigos PKCE antes de montar
a URL de autorização). Emissor: `https://auth.openai.com`.

### 10. O callback loopback é dinâmico ou fixo?

**Fixo, e é essa a resposta que fecha a porta.** O código oficial define porta padrão
`1455` e uma porta de fallback `1457`, com o comentário: *"Keep in sync with the Codex CLI
Hydra redirect URI allow-list."* Ou seja, as URIs de redirecionamento aceitas são uma
allow-list mantida **no lado da OpenAI**, atrelada ao cliente OAuth do Codex. Um
aplicativo de terceiro não consegue registrar a própria URI de callback (nem escolher uma
porta livre, como manda a boa prática de OAuth em loopback) porque não existe registro de
cliente para ele.

### 11. O client ID pode ser usado por terceiros?

**Não.** O client ID do Codex é uma constante pública no código-fonte oficial, mas ser
público não é o mesmo que ser reutilizável: ele identifica o **aplicativo Codex** perante
a OpenAI, está amarrado à allow-list de redirect acima e existe uma variável de ambiente
de override destinada a cenários internos/de teste, não a rebranding.

Usá-lo no Helppye seria fazer o Helppye se passar pelo Codex — exatamente a prática que a
própria instrução desta tarefa proíbe ("não reutilize client secrets, client IDs ou tokens
pertencentes a outro aplicativo"). O princípio não muda quando o outro aplicativo é da
OpenAI em vez do Perssua.

### 12. Há documentação ou termos permitindo uso de ferramentas de desenvolvimento?

Há documentação permitindo **integrar o Codex** (App Server, Codex SDK, MCP) — inclusive
com o pedido de identificar-se via `clientInfo` e de contatar a OpenAI para integrações
corporativas. Não há documentação permitindo **falar diretamente com a autenticação e o
backend do Codex** por fora do Codex.

### 13. O conteúdo de reuniões pode ser enviado por esse canal?

**É a pergunta em aberto mais séria, e a razão de o caminho via App Server ficar em
"Needs official clarification".** O canal é um agente de código: o produto, a
documentação, os limites do plano e a política de retenção estão descritos em torno de
tarefas de engenharia de software. O Helppye enviaria transcrição de fala de **outra
pessoa**, capturada da saída de áudio do sistema — dado de terceiro, possivelmente
sensível, frequentemente sujeito a consentimento de gravação.

Além da questão contratual, há a de expectativa do usuário: quem conecta a conta ChatGPT
para "usar minha assinatura" não espera que a fala do interlocutor entre no mesmo produto
onde ele revisa código da empresa, sob as regras de retenção do workspace dele. Isso
precisa de resposta oficial da OpenAI, não de inferência nossa.

### 14. Existe risco de bloqueio, violação contratual ou dependência de endpoint privado?

**Sim, nos três.**

- **Endpoint privado:** com autenticação por conta ChatGPT, o Codex fala com
  `https://chatgpt.com/backend-api/codex`, que não faz parte da API pública documentada
  (`api.openai.com/v1`). O código oficial inclui até tratamento de cookies do Cloudflare
  para esse host — sinal claro de superfície protegida contra clientes não previstos.
  Construir sobre ele é depender de algo que pode mudar sem aviso e sem deprecação.
- **Contratual:** os termos proíbem compartilhar credenciais e circundar limites de uso;
  usar o client ID do Codex para consumir a assinatura por fora do Codex tem cara de
  exatamente isso.
- **Bloqueio:** o risco recai sobre a **conta do usuário**, não sobre nós. Um produto que
  pode fazer a pessoa perder a assinatura dela não é uma funcionalidade aceitável.

## 3. Decisão

1. **Não implementar** `ChatGptCodexResponseProvider` falando OAuth/HTTP diretamente com
   `auth.openai.com` e `chatgpt.com/backend-api/codex`. Falha em três dos critérios da
   regra de decisão: client ID apropriado indevidamente, endpoint não documentado e
   permissão contratual ausente.
2. **Não implementar**, nesta execução, o caminho via Codex App Server. Ele é oficial e
   tecnicamente viável, mas depende de um binário externo instalado e logado, entrega um
   agente de código onde o Helppye precisa de três frases com baixa latência, consome a
   cota de Codex do usuário e deixa em aberto a pergunta 13.
3. **Deixar a extensão arquitetural pronta.** `ResponseProviderId::ChatGptCodexAccount`
   existe no enum e aparece em `response_provider::registry::PLANNED` com
   `available = false` e um motivo que aponta para este ADR. Nenhum código de produção
   tenta autenticar, e nenhum endpoint foi inventado: a UI pode listar o provedor como
   indisponível e explicar por quê, em vez de fingir que ele funciona.
4. **Não implementar a Parte 11 (segurança OAuth).** Sem fluxo OAuth, não há listener
   loopback, `state`, PKCE ou refresh token para proteger. Se o item 2 destravar um dia,
   a Parte 11 volta a valer integralmente — e, no caminho via App Server, boa parte dela
   deixa de ser nossa: quem faz PKCE, guarda e renova token é o Codex.

## 4. O que destravaria esta decisão

Qualquer um destes, por escrito e em fonte oficial:

- registro de cliente OAuth público em `auth.openai.com` para aplicativos nativos de
  terceiros, com redirect URI de loopback dinâmico próprio;
- declaração oficial de que consumir a assinatura ChatGPT via Codex App Server, em um
  produto de terceiros, é permitido para conteúdo que não é código;
- uma API pública e documentada que aceite a credencial de assinatura para geração de
  texto genérica.

Enquanto nada disso existir, a recomendação para quem quer usar nuvem continua sendo a que
já está implementada: API key própria (OpenAI, DeepSeek, Anthropic, OpenRouter, ou
endpoint compatível), com a chave no keychain do SO. E o padrão continua sendo local.

## 5. Fontes consultadas

- Codex — Authentication: <https://developers.openai.com/codex/auth>
  (redireciona para <https://learn.chatgpt.com/docs/auth.md>)
- Codex — App Server: <https://developers.openai.com/codex/app-server>
- Codex — CLI reference: <https://developers.openai.com/codex/cli/reference>
- Código oficial (Apache-2.0), lido via API do GitHub:
  - `codex-rs/login/src/server.rs` — emissor `https://auth.openai.com`, portas 1455/1457,
    comentário sobre a allow-list de redirect URI, uso de PKCE;
  - `codex-rs/login/src/auth/manager.rs` — `client_id` constante do Codex, endpoints
    `oauth/token` e `oauth/revoke`, variáveis de override;
  - `codex-rs/model-provider-info/src/lib.rs` — `https://chatgpt.com/backend-api/codex`;
  - `codex-rs/app-server/README.md` — protocolo JSON-RPC, primitivas de agente, seção
    "Auth endpoints" e modos de autenticação.
- OpenAI — Terms of Use: <https://openai.com/policies/row-terms-of-use/>
- OpenAI — Services Agreement: <https://openai.com/policies/services-agreement/>
- OpenAI — Usage policies: <https://openai.com/policies/usage-policies/>
