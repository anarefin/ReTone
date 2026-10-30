import { useRef, type ComponentType, type KeyboardEvent } from "react";

export interface SettingsTab<Id extends string> {
  id: Id;
  label: string;
  Icon: ComponentType<{ className?: string }>;
}

interface SettingsSidebarProps<Id extends string> {
  tabs: SettingsTab<Id>[];
  selectedId: Id;
  onSelect: (id: Id) => void;
  label: string;
}

export const settingsTabId = (id: string) => `settings-tab-${id}`;
export const settingsPanelId = (id: string) => `settings-panel-${id}`;

// Vertical tablist; Up/Down/Home/End move the selection and focus with it.
export default function SettingsSidebar<Id extends string>({
  tabs,
  selectedId,
  onSelect,
  label,
}: SettingsSidebarProps<Id>) {
  const listRef = useRef<HTMLDivElement>(null);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const current = tabs.findIndex((tab) => tab.id === selectedId);
    let next: number;
    switch (event.key) {
      case "ArrowDown":
        next = (current + 1) % tabs.length;
        break;
      case "ArrowUp":
        next = (current - 1 + tabs.length) % tabs.length;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = tabs.length - 1;
        break;
      default:
        return;
    }
    event.preventDefault();
    onSelect(tabs[next].id);
    listRef.current?.querySelector<HTMLElement>(`#${settingsTabId(tabs[next].id)}`)?.focus();
  };

  return (
    <div
      ref={listRef}
      role="tablist"
      aria-orientation="vertical"
      aria-label={label}
      onKeyDown={onKeyDown}
      className="flex flex-col gap-0.5"
    >
      {tabs.map(({ id, label: tabLabel, Icon }) => {
        const isSelected = id === selectedId;
        return (
          <button
            key={id}
            id={settingsTabId(id)}
            type="button"
            role="tab"
            aria-selected={isSelected}
            aria-controls={settingsPanelId(id)}
            tabIndex={isSelected ? 0 : -1}
            onClick={() => onSelect(id)}
            className={`flex items-center gap-2 rounded-md px-2.5 py-1.5 text-start text-sm font-medium outline-none transition-colors duration-100 focus-visible:ring-2 focus-visible:ring-ring/30 ${
              isSelected
                ? "bg-muted/60 dark:bg-surface-2 text-foreground"
                : "text-muted-foreground hover:text-foreground"
            }`}
          >
            <Icon className={`h-4 w-4 shrink-0 ${isSelected ? "text-primary" : ""}`} />
            <span className="truncate">{tabLabel}</span>
          </button>
        );
      })}
    </div>
  );
}
