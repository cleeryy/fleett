import { AgentWizard } from "@/components/wizard/agent-wizard";
import { ExpertInstancePage } from "@/pages/instance-new-expert";
import { useRoute } from "@/lib/router";

/**
 * Creation entry point (#72). The guided four-step wizard is the default; the
 * expert form keeps every field and lives at `#/instances/new?form=expert`.
 * Both paths share one draft and send the same `CreateInstanceInput`.
 */
export function NewInstancePage() {
  const route = useRoute();
  if (route.query.get("form") === "expert") return <ExpertInstancePage />;
  return <AgentWizard />;
}
