/** Fiche agent (#73) : onglets, vue d'ensemble, panneau de création, vue avancée. */
export const detailFr: Record<string, string> = {
  "agent.tab.configure": "Configurer",
  "agent.tab.activity": "Activité",
  "agent.tab.advanced": "Avancé",

  "agent.header.noPersona": "Aucune personnalité",
  "agent.header.lastActivity": "dernière activité {when}",
  "agent.header.copyId": "identifiant de l'agent",

  "agent.action.personality": "Modifier la personnalité",
  "agent.action.addSkill": "Ajouter une compétence",
  "agent.action.viewLogs": "Voir les logs",
  "agent.action.configure": "Voir la configuration",

  "agent.overview.problemsTitle": "Problèmes en cours",
  "agent.overview.noProblems": "Aucun problème détecté.",
  "agent.overview.whatTitle": "Ce que fait cet agent",
  "agent.overview.whatDescription":
    "La personnalité qu'il suit, le gabarit de départ, le modèle et le canal sur lesquels il répond.",
  "agent.overview.noPersona": "Aucune personnalité : le SOUL.md du gabarit est utilisé tel quel.",
  "agent.overview.template": "Gabarit",
  "agent.overview.nextRunsTitle": "Prochaines exécutions planifiées",
  "agent.overview.manageSchedules": "Gérer les planifications",
  "agent.overview.nextRunsEmpty":
    "Aucune exécution planifiée. Une planification réveille l'agent sur une cadence cron avec une consigne.",
  "agent.overview.messagesTitle": "Derniers messages",
  "agent.overview.openMessages": "Ouvrir les messages",
  "agent.overview.messagesEmpty":
    "Aucun message pour l'instant. L'agent apparaîtra ici dès qu'il échangera avec un autre agent.",
  "agent.overview.viewerHint":
    "Votre compte est en lecture seule : la configuration, les fichiers et les messages sont visibles mais non modifiables.",

  "agent.created.title": "{name} est prêt",
  "agent.created.description":
    "L'agent est créé et son répertoire de données est en place. Voici la prochaine étape.",
  "agent.created.discord": "Tester sur Discord",
  "agent.created.allowedUsers": "Choisir qui peut lui parler",
  "agent.created.dismiss": "Masquer",

  "agent.activity.view": "Vue de l'activité",

  "agent.advancedBannerTitle": "Section avancée",
  "agent.advancedBanner":
    "Cette section fait partie de la vue avancée. Passez en mode Avancé pour la retrouver dans la barre d'onglets.",
  "agent.advancedSwitch": "Passer en mode Avancé",
  "agent.advanced.schedules": "Exécutions planifiées",
  "agent.advanced.schedulesDescription":
    "Liste en lecture seule. Créez, modifiez et lancez les planifications depuis la page Planifications.",
  "agent.advanced.manageSchedules": "Ouvrir les planifications",
  "agent.advanced.paused": "en pause",
  "agent.advanced.backupsDescription":
    "Instantanés du répertoire de données : à prendre avant un changement risqué, à restaurer si besoin.",
  "agent.advanced.connectors": "Connecteurs MCP",
  "agent.advanced.connectorsDescription":
    "Serveurs MCP déployés et rattachés à cet agent. En rattacher un réécrit config.yaml et recrée le conteneur.",
  "agent.advanced.accessDescription":
    "Qui peut lire ou piloter cet agent : entrées ACL par utilisateur ou jeton.",
  "agent.advanced.networkDescription":
    "Politique de destination et accès LAN pour la sortie réseau de cet agent.",
  "agent.advanced.memoryDescription":
    "Entrées que l'agent conserve dans son propre namespace de mémoire.",
  "agent.advanced.terminalDescription":
    "Commande unique dans le conteneur de l'agent, avec un délai maximal.",
  "agent.advanced.composeDescription": "Le compose.yaml rendu par le contrôleur.",
};
