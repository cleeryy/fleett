# hermes-fleet — API v1

Base : `http://127.0.0.1:8090`. Toutes les routes `/api/*` exigent
`Authorization: Bearer <FLEET_TOKEN>`, sauf `GET /api/health`.

Erreurs : `{"error":{"code":"...","message":"..."}}` avec le statut HTTP correspondant
(`400` entrée invalide, `401` jeton absent/faux, `404` inconnu, `409` conflit,
`422` YAML/compose invalide, `502` échec Docker).

## Santé et auth

| Méthode | Route | Réponse |
|---|---|---|
| GET | `/api/health` | `{"ok":true,"version":"0.1.0","driver":"mock\|compose","docker":{"ok":true}}` |
| GET | `/api/auth/check` | `204` si le jeton est valide |

## Templates

`GET /api/templates`
```json
[{"id":"guest","name":"Invité","description":"Web + mémoire, aucun accès infra",
  "default_model":"deepseek-v4.1-flash","channel":"telegram"}]
```

## Instances

`GET /api/instances` → `InstanceSummary[]`
```json
{"id":"alice","name":"Alice","template":"guest","model":"deepseek-v4.1-flash",
 "channel":"telegram","status":"running","shares":["findings"],
 "created_at":"2026-11-08T01:00:00Z"}
```

`POST /api/instances` → `201 InstanceDetail`
```json
{"id":"alice","name":"Alice","template":"guest",
 "model":"deepseek-v4.1-flash",
 "base_url":"https://opencode.ai/inference/go/openai/v1",
 "api_key_env":"HERMES_GATE_PROFILE_TOKEN",
 "channel":{"type":"telegram","token":"123:ABC"},
 "shares":[{"name":"findings","mode":"rw"}],
 "mem_limit_mb":1536,"notes":""}
```
`id` : `[a-z0-9-]{2,32}`, unique. `channel.type` : `telegram`, `discord` ou `none`.
Le token n'est **jamais** renvoyé par l'API.

`GET /api/instances/{id}` → `InstanceDetail`
```json
{"id":"alice","name":"Alice","template":"guest","model":"deepseek-v4.1-flash",
 "base_url":"https://opencode.ai/inference/go/openai/v1",
 "api_key_env":"HERMES_GATE_PROFILE_TOKEN","channel":"telegram",
 "status":"running","shares":[{"name":"findings","mode":"rw"}],
 "mem_limit_mb":1536,"notes":"","created_at":"...","updated_at":"...",
 "last_error":null,"data_dir":"/opt/hermes-fleet/instances/alice/data",
 "env_keys":["HERMES_GATE_PROFILE_TOKEN","TELEGRAM_BOT_TOKEN"],
 "container":{"id":"3f2a…","image":"hermes-agent:0.22.2-local","uptime_s":3600},
 "compose_yaml":"name: hermes-alice\n…"}
```

`PATCH /api/instances/{id}` → `200 InstanceDetail`. Champs modifiables : `name`, `model`,
`base_url`, `api_key_env`, `channel` (`token` optionnel = rotation), `shares`,
`mem_limit_mb`, `notes`. Réécrit le compose et fait `up -d`.

`POST /api/instances/{id}/start|stop|restart` → `200 {"status":"running|stopped"}`
`DELETE /api/instances/{id}` → `204` (données conservées) · `?purge=true` efface `data/`.
`GET /api/instances/{id}/logs?tail=200` → `text/plain`

## Shares

| Méthode | Route | Corps | Réponse |
|---|---|---|---|
| GET | `/api/shares` | — | `[{"name":"findings","path":"…","instances":[{"id":"alice","mode":"rw"}]}]` |
| POST | `/api/shares` | `{"name":"findings"}` | `201` |
| DELETE | `/api/shares/{name}` | — | `204` · `409` si encore attaché |
| POST | `/api/shares/{name}/attach` | `{"instance":"alice","mode":"rw\|ro"}` | `204` |
| POST | `/api/shares/{name}/detach` | `{"instance":"alice"}` | `204` |

## Statuts

`created` · `running` · `stopped` · `error` · `unknown`

`status` est un cache : la valeur fait foi est relue via `docker compose ps` à chaque
`GET /api/instances` (driver `compose`). En driver `mock`, l'état est simulé.
