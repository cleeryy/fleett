import { InstanceFiles } from "@/components/instance-files";

/**
 * Files tab of the agent detail (#73). Thin wrapper: the editor keeps its own
 * draft, route guard and assistant layer; the page only owns the keep-alive
 * mount so an unsaved buffer survives a trip to another tab.
 */
export function AgentFilesPanel({
  id,
  canEdit,
  active,
  onRestarted,
  onFileSaved,
}: {
  id: string;
  canEdit: boolean;
  active: boolean;
  onRestarted: () => void;
  onFileSaved: () => void;
}) {
  return (
    <InstanceFiles
      id={id}
      canEdit={canEdit}
      active={active}
      onRestarted={onRestarted}
      onFileSaved={onFileSaved}
    />
  );
}
