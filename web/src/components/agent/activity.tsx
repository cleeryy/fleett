import { InstanceMessagesTab } from "@/components/instance-bus";
import { InstanceHistory } from "@/components/instance-history";
import { InstanceLogs } from "@/components/instance-logs";
import { InstanceIncidents } from "@/components/instance-ops";
import { Segmented } from "@/components/segmented";
import {
  ACTIVITY_LABEL_KEYS,
  ACTIVITY_VIEWS,
  type ActivityView,
} from "@/lib/agent-tabs";
import { useI18n } from "@/lib/i18n";

/**
 * Activity tab of the agent detail (#73): one place for what the agent did and
 * what it is doing. The segment lives in `?view=` so a deep link
 * (`?tab=logs`, `?tab=incidents`, …) opens the right panel.
 */
export function AgentActivityPanel({
  id,
  canEdit,
  view,
  onViewChange,
}: {
  id: string;
  canEdit: boolean;
  view: ActivityView;
  onViewChange: (next: ActivityView) => void;
}) {
  const { t } = useI18n();
  return (
    <div className="space-y-4">
      <div className="overflow-x-auto">
        <Segmented
          value={view}
          onChange={onViewChange}
          label={t("agent.activity.view")}
          options={ACTIVITY_VIEWS.map((candidate) => ({
            value: candidate,
            label: t(ACTIVITY_LABEL_KEYS[candidate]),
          }))}
        />
      </div>

      {view === "logs" ? <InstanceLogs id={id} /> : null}
      {view === "incidents" ? <InstanceIncidents id={id} /> : null}
      {view === "messages" ? <InstanceMessagesTab id={id} canEdit={canEdit} /> : null}
      {view === "history" ? <InstanceHistory id={id} canEdit={canEdit} /> : null}
    </div>
  );
}
