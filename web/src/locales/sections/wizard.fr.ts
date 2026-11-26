/** Assistant de création d'agent (#72). */
export const wizardFr: Record<string, string> = {
  // ── Page et progression ──────────────────────────────────────────────
  "wizard.title": "Créer un agent",
  "wizard.description":
    "Quatre étapes pour lancer un agent. Vos réponses restent dans ce navigateur jusqu'à sa création.",
  "wizard.advancedConfig": "Configuration avancée",
  "wizard.guidedConfig": "Assistant guidé",
  "wizard.progressAria": "Progression de la création",
  "wizard.saved": "Brouillon enregistré",
  "wizard.submit": "Créer et démarrer",
  "wizard.created": "Agent {id} créé.",
  "wizard.mcpAttachFailed":
    "L'agent a été créé, mais le serveur MCP {server} n'a pas pu être attaché : {message}",

  "wizard.resume.aria": "Reprendre la création",
  "wizard.resume.title": "Reprendre la création de {name} ?",
  "wizard.resume.resume": "Reprendre",
  "wizard.resume.restart": "Recommencer",
  "wizard.resume.unnamed": "cet agent",
  "wizard.resume.secrets":
    "Les jetons ne sont jamais enregistrés : ressaisissez-les à l'étape Connexion si besoin.",

  // ── Étapes ───────────────────────────────────────────────────────────
  "wizard.step.purpose.short": "Usage",
  "wizard.step.purpose.title": "À quoi sert-il ?",
  "wizard.step.purpose.description":
    "Choisissez l'usage le plus proche. Il détermine un modèle d'agent ; tout reste modifiable ensuite.",

  "wizard.step.identity.short": "Identité",
  "wizard.step.identity.title": "Son identité",
  "wizard.step.identity.description":
    "Un nom reconnaissable, un identifiant pour l'hôte, et la personnalité de l'agent.",

  "wizard.step.connection.short": "Connexion",
  "wizard.step.connection.title": "Sa connexion",
  "wizard.step.connection.description":
    "Le justificatif du modèle qu'il appelle, et la façon dont vous lui parlez.",

  "wizard.step.review.short": "Vérification",
  "wizard.step.review.title": "Vérification",
  "wizard.step.review.description":
    "Vérifiez l'essentiel, puis créez. Les options avancées sont à un clic.",

  // ── Cartes d'usage ───────────────────────────────────────────────────
  "wizard.usage.aria": "À quoi sert cet agent ?",
  "wizard.usage.discord.title": "Assistant Discord",
  "wizard.usage.discord.hint":
    "Répond à votre équipe dans un serveur Discord, avec son propre bot et son jeton.",
  "wizard.usage.watch.title": "Veille / résumé",
  "wizard.usage.watch.hint":
    "Lit des sources à intervalle régulier et conserve un résumé court et durable.",
  "wizard.usage.dev.title": "Développement",
  "wizard.usage.dev.hint":
    "Exécute du code, inspecte des dépôts et aide sur les tâches d'ingénierie.",
  "wizard.usage.other.title": "Autre",
  "wizard.usage.other.hint": "Partez d'un modèle d'agent et ajustez-le vous-même.",
  "wizard.usage.otherTemplate": "Modèle d'agent",
  "wizard.usage.otherTemplatePlaceholder": "Choisir un modèle",
  "wizard.usage.otherTemplateHint":
    "Le modèle d'agent fournit la configuration, le `SOUL.md` de départ et le modèle par défaut.",
  "wizard.usage.selected": "Modèle {name} · modèle par défaut {model}.",
  "wizard.usage.noTemplatesLead":
    "Aucun modèle d'agent n'est encore disponible. Ajoutez-en depuis",
  "wizard.usage.noTemplatesAction": "Modèles d'agent",
  "wizard.usage.noTemplatesTail": "sur l'hôte, puis rechargez cette page.",

  // ── Erreurs de champ ─────────────────────────────────────────────────
  "wizard.errors.templateRequired": "Choisissez un usage ou un modèle d'agent.",
  "wizard.errors.nameRequired": "Saisissez un nom.",
  "wizard.errors.nameTooLong": "Utilisez 48 caractères au maximum.",
  "wizard.errors.idRequired": "Le nom doit produire un identifiant.",
  "wizard.errors.idFormat":
    "Utilisez 2 à 32 caractères : lettres minuscules, chiffres, tirets.",
  "wizard.errors.idReserved": "Cet identifiant est réservé.",
  "wizard.errors.modelRequired": "Saisissez un modèle.",
  "wizard.errors.tokenRequired": "Saisissez le jeton du bot.",
  "wizard.errors.memoryRange": "Utilisez 128 à 8192 Mo.",
  "wizard.errors.tooManySkills": "Un agent monte au maximum {max} compétences.",

  // ── Identité ─────────────────────────────────────────────────────────
  "wizard.identity.name": "Nom de l'agent",
  "wizard.identity.namePlaceholder": "Alice",
  "wizard.identity.slug": "Identifiant",
  "wizard.identity.slugHint":
    "Dérivé du nom : nomme le dossier du conteneur `instances/{id}` et le projet compose.",
  "wizard.identity.language": "Langue des réponses",
  "wizard.identity.languageHint":
    "Écrite dans la personnalité lorsque celle-ci expose une variable de langue.",
  "wizard.language.fr": "Français",
  "wizard.language.en": "English",
  "wizard.language.valueFr": "français",
  "wizard.language.valueEn": "English",
  "wizard.identity.persona": "Personnalité",
  "wizard.identity.personaHint":
    "Rend un `SOUL.md` réutilisable à la création. Les valeurs restent sur cet agent.",
  "wizard.identity.managePersonas": "Gérer les personnalités",
  "wizard.identity.personaVarsOne": "{count} variable à renseigner",
  "wizard.identity.personaVarsMany": "{count} variables à renseigner",
  "wizard.identity.values": "Valeurs pour cet agent",
  "wizard.identity.preview": "Aperçu de la personnalité",
  "wizard.identity.previewHint":
    "Rendu depuis la personnalité enregistrée ; stocké sur l'agent à la création.",
  "wizard.identity.noPersona":
    "Aucune personnalité : l'agent garde le SOUL.md fourni par son modèle. Vous pourrez en assigner une plus tard dans Configuration.",
  "wizard.identity.notes": "Notes",
  "wizard.identity.notesPlaceholder": "À qui sert cet agent, ce qu'il peut toucher.",

  // ── Connexion ────────────────────────────────────────────────────────
  "wizard.connection.provider": "Fournisseur de modèles",
  "wizard.connection.providerHint":
    "Partagez un justificatif et héritez de son URL de base et du nom de variable. Le modèle reste propre à l'agent.",
  "wizard.connection.providersLoadFailed":
    "Impossible de charger les fournisseurs : {message}",
  "wizard.connection.usesProvider": "Utilise votre fournisseur {name}",
  "wizard.connection.test": "Tester",
  "wizard.connection.testing": "Test en cours…",
  "wizard.connection.testOk": "Test OK · {ms} ms",
  "wizard.connection.testFailed": "Test échoué",
  "wizard.connection.noProviderTitle": "Aucun fournisseur de modèles",
  "wizard.connection.noProviderHint":
    "Un agent a besoin d'un justificatif pour appeler son modèle. Créez un fournisseur partagé une fois ; chaque agent pourra le réutiliser.",
  "wizard.connection.createProvider": "Créer un fournisseur",
  "wizard.connection.noProviderContinue":
    "Vous pouvez continuer : l'agent sera créé, mais ne pourra pas répondre tant qu'aucun fournisseur ne sera rattaché.",
  "wizard.connection.noProviderSelected":
    "Aucun fournisseur choisi : l'agent stocke son propre justificatif dans les options avancées.",
  "wizard.connection.model": "Modèle",
  "wizard.connection.modelHint":
    "Le modèle par défaut du modèle d'agent ; saisissez n'importe quel modèle servi par votre fournisseur.",
  "wizard.connection.channel": "Connecteur",
  "wizard.channel.discord": "Discord",
  "wizard.channel.discordHint": "Bot dans un serveur Discord",
  "wizard.channel.telegram": "Telegram",
  "wizard.channel.telegramHint": "Bot dans Telegram",
  "wizard.channel.none": "Aucun connecteur",
  "wizard.channel.noneHint": "Console et API uniquement",
  "wizard.channelSummary.discord": "assistant Discord",
  "wizard.channelSummary.telegram": "assistant Telegram",
  "wizard.channelSummary.none": "agent console",
  "wizard.connection.botToken": "Jeton du bot",
  "wizard.connection.botTokenHint":
    "Chiffré au repos. L'API ne le renvoie jamais. Un nouvel agent ne peut pas réutiliser un jeton déjà stocké.",

  // ── Vérification ─────────────────────────────────────────────────────
  "wizard.review.summaryAria": "Récapitulatif",
  "wizard.review.personality": "Personnalité",
  "wizard.review.overrides": "{count} valeur(s) remplacée(s)",
  "wizard.review.nonePersona": "Aucune — garde le SOUL.md du modèle",
  "wizard.review.channel": "Connecteur",
  "wizard.review.model": "Modèle",
  "wizard.review.viaProvider": "via {name}",
  "wizard.review.template": "Modèle d'agent",
  "wizard.review.noneGeneric": "Aucun",
  "wizard.review.skills": "Compétences",
  "wizard.review.shares": "Dossiers partagés",
  "wizard.review.latest": "dernière",
  "wizard.review.mcp": "Serveur MCP",
  "wizard.review.mcpHint":
    "Ce serveur MCP sera attaché juste après la création de l'agent.",
  "wizard.review.advanced": "Options avancées",
  "wizard.review.advancedHint":
    "Mémoire, réseau, environnement et montages. Le formulaire expert conserve tous les champs.",
  "wizard.review.memory": "Limite de mémoire (Mo)",
  "wizard.review.memoryHint": "128 à 8192 Mo. 1536 Mo convient à un assistant de discussion.",
  "wizard.review.network": "Accès réseau",
  "wizard.review.gateToken": "Justificatif du fournisseur",
  "wizard.review.gateTokenHint":
    "Uniquement sans fournisseur rattaché. Chiffré au repos ; l'hôte a besoin de sa clé de secrets (`sudo fleett secrets init`).",
  "wizard.units.gb": "Go",
};
