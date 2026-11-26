/**
 * Product copy lane (#75): glossary, onboarding, empty states, error wording.
 * Every key here is new: base `en.ts` keeps the older surface. `copy.fr.ts`
 * must expose the same key set.
 */
export const copyEn: Record<string, string> = {
  // ── First-sign-in questionnaire ───────────────────────────────────────
  "onboarding.title": "Welcome to fleett",
  "onboarding.description":
    "Three answers to tune the console to the way you work. The detail level stays changeable at any time.",
  "onboarding.goal.title": "What do you want to do first?",
  "onboarding.goal.create": "Create an assistant",
  "onboarding.goal.explore": "Explore",
  "onboarding.goal.administer": "Administer",
  "onboarding.scale.title": "How many agents do you expect?",
  "onboarding.scale.one": "One",
  "onboarding.scale.few": "A few",
  "onboarding.scale.many": "Many",
  "onboarding.mode.title": "How much detail?",
  "onboarding.mode.simple": "Simple",
  "onboarding.mode.advanced": "Advanced",
  "onboarding.mode.recommended": " (recommended)",
  "onboarding.mode.hint":
    "Simple mode shows the essentials; advanced mode reveals automation and system tools. You can switch at any time.",
  "onboarding.submit": "Continue",

  // ── First-agent checklist ─────────────────────────────────────────────
  "checklist.title": "Your first agent in four steps",
  "checklist.remainingOne": "{count} step left before an agent can answer.",
  "checklist.remainingMany": "{count} steps left before an agent can answer.",
  "checklist.allDone": "All set: your agent can answer.",
  "checklist.todo": "To do",
  "checklist.dismiss": "Hide getting started",
  "checklist.step.create.title": "Create",
  "checklist.step.create.description": "Start from an agent template, give it a name and pick its model.",
  "checklist.step.create.action": "Create an agent",
  "checklist.step.connect.title": "Connect",
  "checklist.step.connect.description": "Link the agent to Discord or Telegram so you can talk to it.",
  "checklist.step.connect.action": "Configure the connector",
  "checklist.step.test.title": "Test",
  "checklist.step.test.description": "Send a message and check the reply.",
  "checklist.step.test.action": "Test the agent",
  "checklist.step.personalize.title": "Personalize",
  "checklist.step.personalize.description": "Attach a personality or a skill.",
  "checklist.step.personalize.action": "Browse personalities",

  // ── Empty states ──────────────────────────────────────────────────────
  "fleet.emptyCreateAction": "Create an agent",

  // ── Glossary (docs/GLOSSARY.md keeps the FR/EN table) ─────────────────
  "glossary.agent": "Agent",
  "glossary.provider": "Model provider",
  "glossary.persona": "Personality",
  "glossary.skill": "Skill",
  "glossary.connector": "Connector",
  "glossary.share": "Shared folder",
  "glossary.network": "Network access",
  "glossary.snapshot": "Snapshot",
  "glossary.docker": "Docker configuration",
  "glossary.template": "Agent template",
};
