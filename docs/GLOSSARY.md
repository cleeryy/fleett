# Glossary — business vocabulary for the console

The simple console speaks about agents and assistants, not about containers and
compose projects. These ten terms are the contract between the interface copy,
the onboarding questionnaire, the first-agent checklist and the empty states.
Technical pages (logs, compose, egress, MCP, audit) keep their technical
vocabulary: renaming them would make the product harder to operate, not easier.

| Français | English | Never shown in Simple mode |
|---|---|---|
| Agent | Agent | instance, conteneur, projet compose |
| Fournisseur de modèles | Model provider | provider, base URL, clé d'API |
| Personnalité | Persona | persona, `SOUL.md`, variables `{{clé}}` |
| Compétence | Skill | skill, `SKILL.md`, script de compétence |
| Connecteur | Connector | canal, `channel`, jeton de bot |
| Dossier partagé | Shared folder | share, volume, montage |
| Accès réseau | Network access | egress, politique réseau, proxy, LAN |
| Sauvegarde instantanée | Snapshot | snapshot, restauration |
| Configuration Docker | Docker configuration | compose, image, conteneur Docker |
| Modèle d'agent | Agent template | template, `config.yaml`, `template.yaml` |

## Rules

- One term, one meaning. « Instance » is an API-level word; the console says
  **agent**. A **modèle d'agent** is the starting point of an agent; a
  **fournisseur de modèles** is the account and endpoint that answer model
  calls. The two are never interchangeable.
- A **connecteur** is how an agent talks to the outside world (a Discord or
  Telegram bot today). A **compétence** is a capability the agent loads; a
  **personnalité** is who it is.
- **Accès réseau** covers both directions: reaching the LAN and reaching the
  internet through a proxy. The advanced pages keep the words « egress » and
  « LAN » because operators configure them by those names.
- Error messages explain the cause and the fix in one sentence, then keep the
  technical code in the details — never the other way around.
- Documentation, API payloads and controller messages stay in their technical
  wording. This glossary only governs interface copy.

The FR/EN key set lives in `web/src/locales/sections/copy.{en,fr}.ts`
(`glossary.*`), so the table and the dictionary cannot drift apart.
