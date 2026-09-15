import {
  makeStyles,
  tokens,
  Tooltip,
} from "@fluentui/react-components";
import type { ComponentType } from "react";
import {
  ClipboardPasteRegular,
  CameraRegular,
  TranslateRegular,
  GlobeRegular,
  ShieldRegular,
  FolderRegular,
  DesktopRegular,
  KeyboardRegular,
  DocumentRegular,
  NotebookRegular,
  CodeRegular,
  SettingsRegular,
  FlashRegular,
} from "@fluentui/react-icons";
import { MODULES, MODULE_GROUPS, type ModuleDef } from "./modules";

/** 模块导航（docs/DESIGN.md §3.3；P0/P1/P2 分组 + 运行状态点） */
const useStyles = makeStyles({
  root: {
    borderRight: `1px solid ${tokens.colorNeutralStroke2}`,
    padding: "10px 8px",
    overflowY: "auto",
    backgroundColor: tokens.colorNeutralBackground1,
  },
  group: {
    fontSize: tokens.fontSizeBase100,
    color: tokens.colorNeutralForeground3,
    padding: "10px 10px 6px",
    letterSpacing: "0.4px",
    display: "block",
  },
  item: {
    position: "relative",
    display: "flex",
    alignItems: "center",
    gap: "10px",
    width: "100%",
    height: "34px",
    padding: "0 10px",
    borderRadius: tokens.borderRadiusMedium,
    color: tokens.colorNeutralForeground2,
    textAlign: "left",
    backgroundColor: "transparent",
    border: "none",
    cursor: "pointer",
    fontSize: tokens.fontSizeBase300,
    ":hover": {
      backgroundColor: tokens.colorNeutralBackground1Hover,
      color: tokens.colorNeutralForeground1,
    },
  },
  itemActive: {
    backgroundColor: tokens.colorNeutralBackground3,
    color: tokens.colorNeutralForeground1,
    fontWeight: tokens.fontWeightSemibold,
    "::before": {
      content: '""',
      position: "absolute",
      left: "0",
      top: "8px",
      bottom: "8px",
      width: "3px",
      borderRadius: tokens.borderRadiusSmall,
      background: tokens.colorBrandForeground1,
    },
  },
  dot: {
    marginLeft: "auto",
    width: "7px",
    height: "7px",
    borderRadius: "50%",
  },
  dotRun: { background: tokens.colorPaletteGreenForeground1 },
  dotOff: {
    background: tokens.colorPaletteDarkOrangeForeground1,
    marginLeft: "auto",
    width: "7px",
    height: "7px",
    borderRadius: "50%",
  },
  tag: {
    marginLeft: "auto",
    fontSize: tokens.fontSizeBase100,
    color: tokens.colorNeutralForeground3,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusSmall,
    padding: "0 5px",
    lineHeight: "16px",
  },
  icon: { flex: "none", fontSize: "17px" },
});

const ICONS: Record<string, ComponentType<{ className?: string }>> = {
  clipboard: ClipboardPasteRegular,
  screenshot: CameraRegular,
  ocr: TranslateRegular,
  proxy: GlobeRegular,
  vault: ShieldRegular,
  file: FolderRegular,
  desktop: DesktopRegular,
  kvm: KeyboardRegular,
  editor: DocumentRegular,
  notes: NotebookRegular,
  term: CodeRegular,
  sys: SettingsRegular,
  automation: FlashRegular,
};

interface Props {
  active: string;
  onChange: (id: string) => void;
}

export default function ModuleNav({ active, onChange }: Props) {
  const styles = useStyles();
  return (
    <nav className={styles.root} aria-label="模块导航">
      {MODULE_GROUPS.map((g) => (
        <div key={g.phase}>
          <span className={styles.group}>{g.label}</span>
          {MODULES.filter((m) => m.phase === g.phase).map((m: ModuleDef) => {
            const Icon = ICONS[m.id] ?? DocumentRegular;
            const isActive = active === m.id;
            return (
              <Tooltip content={`${m.name}（${m.phase}）`} relationship="label" key={m.id}>
                <button
                  className={`${styles.item} ${isActive ? styles.itemActive : ""}`}
                  onClick={() => onChange(m.id)}
                  aria-current={isActive ? "page" : undefined}
                >
                  <Icon className={styles.icon} />
                  {m.name}
                  {m.running ? (
                    <span className={styles.dotRun} aria-label="运行中" />
                  ) : (
                    <span className={styles.tag}>{m.phase}</span>
                  )}
                </button>
              </Tooltip>
            );
          })}
        </div>
      ))}
    </nav>
  );
}
