/**
 * Copie produit (#75) : glossaire, onboarding, états vides, messages d'erreur.
 * Chaque clé est nouvelle : `fr.ts` conserve l'ancienne surface. Le jeu de clés
 * doit rester identique à `copy.en.ts`.
 */
export const copyFr: Record<string, string> = {
  // ── Questionnaire de première connexion ───────────────────────────────
  "onboarding.title": "Bienvenue dans fleett",
  "onboarding.description":
    "Trois réponses pour adapter la console à votre façon de travailler. Le niveau de détail reste modifiable à tout moment.",
  "onboarding.goal.title": "Que voulez-vous faire en premier ?",
  "onboarding.goal.create": "Créer un assistant",
  "onboarding.goal.explore": "Explorer",
  "onboarding.goal.administer": "Administrer",
  "onboarding.scale.title": "Combien d'agents prévoyez-vous ?",
  "onboarding.scale.one": "Un",
  "onboarding.scale.few": "Quelques-uns",
  "onboarding.scale.many": "Beaucoup",
  "onboarding.mode.title": "Quel niveau de détail ?",
  "onboarding.mode.simple": "Simple",
  "onboarding.mode.advanced": "Avancé",
  "onboarding.mode.recommended": " (recommandé)",
  "onboarding.mode.hint":
    "Le mode simple montre l'essentiel ; le mode avancé révèle l'automatisation et le système. Vous pouvez changer à tout moment.",
  "onboarding.submit": "Continuer",

  // ── Liste des premiers pas de l'agent ─────────────────────────────────
  "checklist.title": "Votre premier agent, en quatre étapes",
  "checklist.remainingOne": "{count} étape restante avant qu'un agent puisse répondre.",
  "checklist.remainingMany": "{count} étapes restantes avant qu'un agent puisse répondre.",
  "checklist.allDone": "Tout est prêt : votre agent peut répondre.",
  "checklist.todo": "À faire",
  "checklist.dismiss": "Masquer les premiers pas",
  "checklist.step.create.title": "Créer",
  "checklist.step.create.description": "Partez d'un modèle d'agent, donnez-lui un nom et choisissez son modèle.",
  "checklist.step.create.action": "Créer un agent",
  "checklist.step.connect.title": "Connecter",
  "checklist.step.connect.description":
    "Reliez l'agent à Discord ou Telegram pour lui parler.",
  "checklist.step.connect.action": "Configurer le connecteur",
  "checklist.step.test.title": "Tester",
  "checklist.step.test.description": "Envoyez un message et vérifiez la réponse.",
  "checklist.step.test.action": "Tester l'agent",
  "checklist.step.personalize.title": "Personnaliser",
  "checklist.step.personalize.description": "Attachez une personnalité ou une compétence.",
  "checklist.step.personalize.action": "Parcourir les personnalités",

  // ── États vides ───────────────────────────────────────────────────────
  "fleet.emptyCreateAction": "Créer un agent",

  // ── Glossaire (table FR/EN dans docs/GLOSSARY.md) ─────────────────────
  "glossary.agent": "Agent",
  "glossary.provider": "Fournisseur de modèles",
  "glossary.persona": "Personnalité",
  "glossary.skill": "Compétence",
  "glossary.connector": "Connecteur",
  "glossary.share": "Dossier partagé",
  "glossary.network": "Accès réseau",
  "glossary.snapshot": "Sauvegarde instantanée",
  "glossary.docker": "Configuration Docker",
  "glossary.template": "Modèle d'agent",
};
