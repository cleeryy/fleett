# hermes-fleet

Console web pour créer, configurer, démarrer/arrêter et supprimer des agents Hermes,
chacun dans son propre conteneur Docker — sans jamais toucher à l'instance existante.

**Statut : en construction.** Voir `SPEC.md` (architecture) et `API.md` (contrat).

```bash
# backend
cd backend && cargo run            # FLEET_DRIVER=mock par défaut

# frontend
cd web && pnpm dev                 # VITE_MOCK=1 par défaut
```
