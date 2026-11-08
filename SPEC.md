# hermes-fleet — spécification

Console web pour **créer, configurer, démarrer/arrêter et supprimer** des agents Hermes,
chacun dans son conteneur Docker. L'instance Hermes existante n'est **jamais** touchée.

## Décisions d'architecture

### 1. Un projet Docker Compose par instance
`instances/<id>/compose.yaml` est écrit sur disque et piloté par
`docker compose -f instances/<id>/compose.yaml -p hermes-<id> …`.
Pas de conteneurs créés à la volée par l'API Docker : le fichier compose est la source de
vérité, lisible et réparable à la main.

### 2. Découplage total (exigence)
`restart: unless-stopped` sur chaque service. Si la web app est arrêtée, mise à jour ou
supprimée, **les agents continuent de tourner**. Si l'hôte redémarre, Docker les relance.
La web app n'est qu'un plan de contrôle : elle ne supervise aucun process.

### 3. Données et secrets par instance
```
/opt/hermes-fleet/
├── fleet.db                       # index SQLite (pas la source de vérité)
├── instances/<id>/
│   ├── compose.yaml
│   ├── data/                      # monté sur /opt/data (HOME Hermes complet)
│   └── secrets.env                # 0600, bot token + jeton de gate
├── shares/<nom>/                  # répertoires partagés entre instances
└── templates/<template>/          # config.yaml + SOUL.md modèles
```
UID/GID fixés à **10000** pour toutes les instances : nécessaire pour que les shares
soient accessibles en écriture par plusieurs conteneurs. Conséquence assumée : l'isolation
entre invités repose sur le conteneur et sur un montage par instance — jamais sur l'UID.

### 4. Shares (montages partagés)
Un share est un répertoire nommé, monté dans N instances :
`shares/<nom>` → `/opt/data/shared/<nom>`, en `rw` ou `ro` selon le lien.
Ajouter/retirer un share réécrit le compose et fait `docker compose up -d` (recréation du
conteneur, données conservées).

### 5. Templates
`templates/<id>/{config.yaml,SOUL.md}`. Le rendu remplace uniquement des variables
explicites (`{{MODEL}}`, `{{BASE_URL}}`, `{{AGENT_NAME}}`). Le reste du fichier est du YAML
Hermes valide, dérivé de `cli-config.yaml.example` (référence locale, hors git).
Aucune génération de config à partir de zéro : un template est relu et versionné.

### 6. Driver
`FLEET_DRIVER=compose` pilote Docker ; `FLEET_DRIVER=mock` (défaut dev/tests) simule les
états sans toucher Docker. Les tests n'appellent jamais Docker.

## Isolation par défaut d'une instance

- réseau bridge dédié `hermes-fleet` (jamais `network_mode: host`)
- aucun montage du socket Docker
- un seul montage de données : `./data:/opt/data`
- `security_opt: no-new-privileges`, `cap_drop: NET_RAW`, `tmpfs: /tmp`
- `pids_limit: 256`, `mem_limit` configurable (défaut 1536m)
- logs json-file plafonnés (10m × 3)

## Cycle de vie

| Action | Effet | Conteneur |
|---|---|---|
| create | écrit compose + data + secrets, `up -d` | running |
| stop | `compose stop` | stopped (ne redémarre pas) |
| start | `compose start` | running |
| restart | `compose restart` | running |
| update | réécrit compose, `up -d` | recréé |
| delete | `compose down` | supprimé, données conservées |
| delete ?purge=true | `compose down` + `rm -rf data/` | supprimé, données effacées |

## Hors périmètre v1

- multi-hôte (un seul démon Docker)
- édition fine de la config Hermes (le dashboard Hermes le fait déjà)
- conversations / chat dans la console
- intégration automatique à `hermes-gate` (les champs existent, le jeton se colle à la main)
- authentification SSO (un jeton bearer suffit pour la v1)
