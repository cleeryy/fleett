/**
 * Highlight a case-insensitive substring inside a log line. The query is
 * escaped, so a message containing `(` or `.*` highlights literally.
 */
export function Highlight({ text, needle }: { text: string; needle: string }) {
  const query = needle.trim();
  if (!query) return <>{text}</>;
  const escaped = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const parts = text.split(new RegExp(`(${escaped})`, "gi"));
  return (
    <>
      {parts.map((part, index) =>
        part.toLowerCase() === query.toLowerCase() ? (
          <mark
            key={`${part}-${index}`}
            className="rounded-[2px] bg-amber-200 px-0.5 text-amber-950 dark:bg-amber-500/40 dark:text-amber-50"
          >
            {part}
          </mark>
        ) : (
          <span key={`${part}-${index}`}>{part}</span>
        ),
      )}
    </>
  );
}
