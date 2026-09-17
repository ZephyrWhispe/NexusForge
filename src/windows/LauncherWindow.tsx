import { useCallback, useEffect, useRef, useState } from "react";
import { makeStyles, tokens, Text, Badge } from "@fluentui/react-components";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  desktopLauncherLaunch,
  desktopLauncherSearch,
  parseAppError,
  type DesktopLauncherHitDto,
} from "../ipc/client";

/**
 * 快速启动器窗口（docs/impl/05 D1+D2）：独立置顶小窗，Alt+Q 全局呼出。
 * 交互：输入即搜（200ms 防抖）；↑↓ 选择、Enter 启动、Esc 隐藏窗口。
 */
const useStyles = makeStyles({
  root: {
    height: "100vh",
    display: "flex",
    flexDirection: "column",
    borderRadius: tokens.borderRadiusXLarge,
    overflow: "hidden",
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
  },
  input: {
    margin: "12px 14px 8px",
    fontSize: tokens.fontSizeBase400,
    padding: "8px 12px",
    borderRadius: tokens.borderRadiusLarge,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
  },
  list: { flex: 1, overflowY: "auto", padding: "4px 8px 10px" },
  item: {
    display: "flex",
    alignItems: "center",
    gap: "10px",
    padding: "10px 12px",
    borderRadius: tokens.borderRadiusLarge,
    cursor: "pointer",
  },
  itemActive: {
    backgroundColor: tokens.colorNeutralBackground3Hover,
  },
  name: { flex: "none", maxWidth: "300px", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" },
  path: {
    flex: 1,
    minWidth: 0,
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
    color: tokens.colorNeutralForeground3,
    fontSize: tokens.fontSizeBase200,
  },
  empty: { padding: "24px", textAlign: "center", color: tokens.colorNeutralForeground3 },
  error: { color: tokens.colorPaletteRedForeground1, fontSize: tokens.fontSizeBase200, padding: "0 16px 8px" },
});

const KIND_LABEL: Record<DesktopLauncherHitDto["kind"], string> = {
  app: "应用",
  action: "动作",
};

export default function LauncherWindow() {
  const styles = useStyles();
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<DesktopLauncherHitDto[]>([]);
  const [active, setActive] = useState(0);
  const [error, setError] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const seq = useRef(0);

  // 搜索（200ms 防抖；空查询列出高频项——频次衰减分兜底排序）
  useEffect(() => {
    const s = ++seq.current;
    const t = setTimeout(() => {
      desktopLauncherSearch(query)
        .then((r) => {
          if (seq.current !== s) return;
          setHits(r);
          setActive(0);
          setError("");
        })
        .catch((e) => {
          if (seq.current !== s) return;
          setHits([]);
          setError(parseAppError(e)?.data.message ?? String(e));
        });
    }, 200);
    return () => clearTimeout(t);
  }, [query]);

  // 窗口显示时聚焦输入框（每次 show 后重新聚焦）
  useEffect(() => {
    const win = getCurrentWindow();
    const focus = () => setTimeout(() => inputRef.current?.focus(), 50);
    focus();
    const p = win.onFocusChanged(({ payload: focused }) => {
      if (focused) focus();
    });
    return () => {
      p.then((u) => u()).catch(() => undefined);
    };
  }, []);

  const launch = useCallback(
    async (hit: DesktopLauncherHitDto) => {
      try {
        await desktopLauncherLaunch(hit.id);
        await getCurrentWindow().hide();
        setQuery("");
      } catch (e) {
        setError(parseAppError(e)?.data.message ?? String(e));
      }
    },
    [],
  );

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      void getCurrentWindow().hide();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((a) => Math.min(a + 1, hits.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => Math.max(a - 1, 0));
    } else if (e.key === "Enter" && hits[active]) {
      void launch(hits[active]);
    }
  };

  // 选中项滚入视野
  useEffect(() => {
    listRef.current
      ?.querySelectorAll(`[data-idx="${active}"]`)[0]
      ?.scrollIntoView({ block: "nearest" });
  }, [active, hits]);

  return (
    <div className={styles.root}>
      <input
        ref={inputRef}
        className={styles.input}
        placeholder="搜索应用与动作…（↑↓ 选择 · Enter 启动 · Esc 关闭）"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={onKeyDown}
        autoFocus
      />
      {error && <div className={styles.error}>{error}</div>}
      <div className={styles.list} ref={listRef}>
        {hits.length === 0 && !error ? (
          <div className={styles.empty}>
            <Text size={300}>
              {query ? "无匹配结果" : "索引构建中或无条目（开始菜单/PATH）"}
            </Text>
          </div>
        ) : (
          hits.map((h, i) => (
            <div
              key={h.id}
              data-idx={i}
              className={`${styles.item} ${i === active ? styles.itemActive : ""}`}
              onMouseEnter={() => setActive(i)}
              onClick={() => void launch(h)}
            >
              <Badge appearance="outline" size="small">
                {KIND_LABEL[h.kind]}
              </Badge>
              <span className={styles.name}>{h.name}</span>
              <span className={styles.path}>{h.path || h.topic || ""}</span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
