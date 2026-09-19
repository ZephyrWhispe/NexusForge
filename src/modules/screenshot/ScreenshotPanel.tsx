import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Badge,
  Button,
  Input,
  makeStyles,
  Text,
  tokens,
} from "@fluentui/react-components";
import {
  screenshotHistoryCopy,
  screenshotHistoryGet,
  screenshotHistoryList,
  screenshotPinGet,
  screenshotPins,
  type PinDataDto,
  type ShotItemDto,
} from "../../ipc/client";
import { notify, reportError } from "../../stores/notifications";
import EmptyState from "../../components/EmptyState";
import DeferredBadge from "../../components/DeferredBadge";

/**
 * 截图与贴图主面板（D-29 B0/T-B0-2）：历史网格（真缩略图，screenshot_history_get 字节出口）
 * + 再复制（CF_DIB 经 ClipboardPort）+ 贴图条。发起截取沿用覆盖层事件通路（overlayController）。
 * 筛选为当前页本地过滤（HistoryQuery 无 text 字段；服务端过滤归 B4）。
 */

const PAGE_SIZE = 30;

const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", flex: 1, minHeight: 0 },
  toolbar: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    height: "40px",
    padding: "0 20px",
    flexShrink: 0,
  },
  spacer: { flex: 1 },
  body: { flex: 1, overflowY: "auto", padding: "8px 20px 20px" },
  sectionTitle: {
    display: "block",
    fontSize: tokens.fontSizeBase200,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorNeutralForeground3,
    padding: "10px 0 6px",
  },
  grid: {
    display: "grid",
    gridTemplateColumns: "repeat(auto-fill, minmax(220px, 1fr))",
    gap: "12px",
  },
  card: {
    display: "flex",
    flexDirection: "column",
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusLarge,
    overflow: "hidden",
    backgroundColor: tokens.colorNeutralBackground1,
  },
  thumb: {
    width: "100%",
    height: "124px",
    objectFit: "cover",
    display: "block",
    backgroundColor: tokens.colorNeutralBackground3,
  },
  thumbMissing: {
    width: "100%",
    height: "124px",
    display: "grid",
    placeItems: "center",
    color: tokens.colorNeutralForeground4,
    fontSize: tokens.fontSizeBase200,
    backgroundColor: tokens.colorNeutralBackground3,
  },
  meta: { padding: "8px 10px 4px", display: "flex", flexDirection: "column", gap: "2px" },
  caption: { fontSize: tokens.fontSizeBase200, color: tokens.colorNeutralForeground2 },
  ocr: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
  },
  ops: { display: "flex", gap: "6px", padding: "4px 10px 10px" },
  pinRow: { display: "flex", gap: "10px", flexWrap: "wrap" },
  pinImg: {
    height: "72px",
    maxWidth: "160px",
    objectFit: "contain",
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground1,
  },
});

/** created_ms → "MM-DD HH:mm"（纯函数，导出供测试钉死） */
export function formatShotTime(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 本地过滤判据：OCR 文本 / 文件名 / 尺寸串，大小写不敏感（纯函数供测试） */
export function shotMatchesFilter(item: ShotItemDto, filter: string): boolean {
  if (!filter.trim()) return true;
  const q = filter.trim().toLowerCase();
  const hay = [item.ocr_text ?? "", item.file ?? "", `${item.width}x${item.height}`].join("\n");
  return hay.toLowerCase().includes(q);
}

export default function ScreenshotPanel() {
  const styles = useStyles();
  const [items, setItems] = useState<ShotItemDto[]>([]);
  const [total, setTotal] = useState(0);
  const [loaded, setLoaded] = useState(false);
  const [filter, setFilter] = useState("");
  const [thumbs, setThumbs] = useState<Record<string, string | null>>({});
  const [pins, setPins] = useState<PinDataDto[]>([]);

  const reload = useCallback(async () => {
    try {
      const [page, pinList] = await Promise.all([
        screenshotHistoryList(1, PAGE_SIZE),
        screenshotPins().catch((e) => {
          reportError(e, { context: "贴图列表加载失败", dedupeKey: "shot-pins", toast: false });
          return [];
        }),
      ]);
      setItems(page.items);
      setTotal(page.total);
      setPins(
        await Promise.all(
          pinList.map(async (p) =>
            screenshotPinGet(p.id)
              .then((d) => [d] as PinDataDto[])
              .catch(() => [] as PinDataDto[]),
          ),
        ).then((rows) => rows.flat()),
      );
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void reload().catch((e) =>
      reportError(e, { context: "截图历史加载失败", dedupeKey: "shot-history" }),
    );
  }, [reload]);

  // 缩略图逐张取字节（410 文件丢失 → null 显示占位，不打扰用户）
  useEffect(() => {
    for (const it of items) {
      if (!it.file || thumbs[it.id] !== undefined) continue;
      screenshotHistoryGet(it.id)
        .then((d) => setThumbs((prev) => ({ ...prev, [it.id]: d.png_b64 })))
        .catch(() => setThumbs((prev) => ({ ...prev, [it.id]: null })));
    }
  }, [items, thumbs]);

  const startCapture = useCallback(() => {
    void import("../../windows/overlayController")
      .then((m) => m.startOverlay("shot"))
      .catch((e) => reportError(e, { context: "发起截取失败", dedupeKey: "shot-start" }));
  }, []);

  const copyImage = useCallback(async (id: string) => {
    try {
      await screenshotHistoryCopy(id);
      notify("success", "已复制到剪贴板", "图片以 CF_DIB 写入，可直接粘贴");
    } catch (e) {
      reportError(e, { context: "再复制失败", dedupeKey: `shot-copy-${id}` });
    }
  }, []);

  const copyText = useCallback((text: string) => {
    void navigator.clipboard.writeText(text).catch((e) => reportError(e, { context: "复制文字失败" }));
  }, []);

  const visible = useMemo(() => items.filter((it) => shotMatchesFilter(it, filter)), [items, filter]);

  if (!loaded) {
    return (
      <div className={styles.root}>
        <EmptyState text="" loading />
      </div>
    );
  }

  return (
    <div className={styles.root}>
      <div className={styles.toolbar}>
        <Button appearance="primary" size="small" onClick={startCapture}>
          开始截取
        </Button>
        <Button size="small" onClick={() => void reload()}>
          刷新
        </Button>
        <Input
          size="small"
          style={{ width: "220px" }}
          placeholder="⌕ 本页筛选（文字/文件名/尺寸）"
          value={filter}
          onChange={(_, d) => setFilter(d.value)}
        />
        <span className={styles.spacer} />
        <DeferredBadge label="录屏" decisionRef="D-08" />
        <DeferredBadge label="每显示器覆盖层" decisionRef="D-23" />
        <Badge appearance="outline">
          {total} 条历史
        </Badge>
      </div>
      <div className={styles.body}>
        {pins.length > 0 && (
          <>
            <Text className={styles.sectionTitle}>当前贴图 · {pins.length}</Text>
            <div className={styles.pinRow}>
              {pins.map((p) => (
                <img
                  key={p.id}
                  className={styles.pinImg}
                  src={`data:image/png;base64,${p.png_b64}`}
                  alt={`贴图 ${p.id}`}
                />
              ))}
            </div>
          </>
        )}
        <Text className={styles.sectionTitle}>截图历史</Text>
        {items.length === 0 ? (
          <EmptyState text="还没有截图记录 · 点『开始截取』创建第一条" />
        ) : visible.length === 0 ? (
          <EmptyState text={`本页 ${items.length} 条中没有匹配「${filter}」的记录`} />
        ) : (
          <div className={styles.grid}>
            {visible.map((it) => (
              <div key={it.id} className={styles.card}>
                {thumbs[it.id] ? (
                  <img
                    className={styles.thumb}
                    src={`data:image/png;base64,${thumbs[it.id]}`}
                    alt={`截图 ${it.width}x${it.height}`}
                  />
                ) : (
                  <div className={styles.thumbMissing}>
                    {it.file ? (thumbs[it.id] === null ? "文件已丢失" : "加载缩略图…") : "未保存文件"}
                  </div>
                )}
                <div className={styles.meta}>
                  <span className={styles.caption} style={{ fontVariantNumeric: "tabular-nums" }}>
                    {it.width}×{it.height} · {formatShotTime(it.created_ms)}
                  </span>
                  {it.ocr_text && <span className={styles.ocr}>{it.ocr_text}</span>}
                </div>
                <div className={styles.ops}>
                  <Button
                    size="small"
                    disabled={!it.file}
                    onClick={() => void copyImage(it.id)}
                  >
                    复制图片
                  </Button>
                  {it.ocr_text && (
                    <Button size="small" onClick={() => copyText(it.ocr_text ?? "")}>
                      复制文字
                    </Button>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
