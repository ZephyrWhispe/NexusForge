import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Badge,
  Button,
  Input,
  Menu,
  MenuButton,
  MenuItem,
  MenuList,
  MenuPopover,
  MenuTrigger,
  makeStyles,
  Switch,
  Text,
  tokens,
} from "@fluentui/react-components";
import {
  hostConfigGet,
  hostConfigSet,
  parseAppError,
  screenshotHistoryCopy,
  screenshotHistoryGet,
  screenshotHistoryList,
  screenshotPinGet,
  screenshotPins,
  screenshotUpload,
  screenshotUploadTargets,
  screenshotWindows,
  type PinDataDto,
  type ShotItemDto,
  type UploadTargetInfoDto,
  type WindowTargetDto,
} from "../../ipc/client";
import { IN_TAURI } from "../../ipc/env";
import { notify, reportError } from "../../stores/notifications";
import { EXPORT_FORMATS, EXPORT_LABEL, type ExportFormat } from "../../windows/overlay/exportFormats";
import BeautifyPopover, { type BeautifyTarget } from "./BeautifyPopover";
import { saveShotAs } from "./saveAs";
import EmptyState from "../../components/EmptyState";
import DeferredBadge from "../../components/DeferredBadge";
import InlineError from "../../components/InlineError";

/**
 * 截图与贴图主面板（D-29 B0/T-B0-2）：历史网格（真缩略图，screenshot_history_get 字节出口）
 * + 再复制（CF_DIB 经 ClipboardPort）+ 贴图条 + 行内美化导出弹层（T-B4-6，预览/另存/复制）。发起截取沿用覆盖层事件通路（overlayController）。
 * 筛选为当前页本地过滤（HistoryQuery 无 text 字段；服务端过滤归 B4）。
 */

const PAGE_SIZE = 30;

/**
 * 「完成后动作链」四枚预设（D-29 B4 T-B4-8）：写入的就是名单本身，
 * 面板不替用户重排顺序——覆盖层「完成」钮按这个次序逐个执行。
 */
export const POST_ACTION_PRESETS: { label: string; actions: string[] }[] = [
  { label: "保存+复制", actions: ["save", "copy"] },
  { label: "仅保存", actions: ["save"] },
  { label: "复制+OCR", actions: ["copy", "ocr"] },
  { label: "贴图+复制", actions: ["pin", "copy"] },
];

/** 盘上的动作名单（坏元素逐个滤掉而不是整份丢弃：一格手打的坏值不该清空其余偏好） */
function readPostActions(cfg: Record<string, unknown>): string[] {
  return Array.isArray(cfg.post_actions)
    ? cfg.post_actions.filter((x): x is string => typeof x === "string")
    : [];
}

/** 读配置里的字符串键（未配置或手改成非字符串一律当空串：面板不该把 `[object Object]` 拼进端点） */
function cfgStr(cfg: Record<string, unknown> | null, key: string): string {
  const v = cfg?.[key];
  return typeof v === "string" ? v : "";
}

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
  chipRow: { display: "flex", gap: "6px", flexWrap: "wrap", alignItems: "center" },
  uploadRow: { display: "flex", gap: "8px", flexWrap: "wrap", alignItems: "center", padding: "4px 0" },
  hint: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    display: "block",
    padding: "6px 0 0",
  },
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
  const [beautify, setBeautify] = useState<BeautifyTarget | null>(null);
  /** 截图配置的整份快照：写回必须是"展开它、只覆一枚键"（host_config_set 整份替换语义） */
  const [shotCfg, setShotCfg] = useState<Record<string, unknown> | null>(null);
  const [postActions, setPostActions] = useState<string[]>([]);
  const [presetBusy, setPresetBusy] = useState<string | null>(null);
  /**
   * 可截取窗口表（T-B4-4）三态：`null` = 还没成功读到（首次打开菜单时现读，
   * 不在面板挂载时就枚举——列别人窗口的标题是跨应用隐私面，不该为"也许会被点开"付费）；
   * `[]` = 读到了，确实没有；`winsErr` 非空 = 读失败（与"没有"分开说，用户才分得清该重试还是该滚开）
   */
  const [wins, setWins] = useState<WindowTargetDto[] | null>(null);
  const [winsErr, setWinsErr] = useState<string | null>(null);
  const [winsOpen, setWinsOpen] = useState(false);
  /** 已注册上传目标（`screenshot_upload_targets` 的读数；空表＝端点没配好，如实反映） */
  const [targets, setTargets] = useState<UploadTargetInfoDto[]>([]);
  /** 端点编辑框：初始跟随盘上的值，保存后由写回的快照对齐（不静默丢掉用户刚打的字） */
  const [endpointDraft, setEndpointDraft] = useState<string | null>(null);
  /** 本次会话的凭据值：只活在这一格 state 里，随组件卸载消失，永不写进配置 */
  const [credValue, setCredValue] = useState("");
  const [uploadBusy, setUploadBusy] = useState<string | null>(null);
  const [lastLink, setLastLink] = useState<string | null>(null);
  /** 后端 apply_config 拒收的那句话（面板原先看不见＝写了不生效也没人说明，T-B4-9 补上） */
  const [cfgRejected, setCfgRejected] = useState<string | null>(null);

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
  // MIME 取宿主嗅探出的 `format`（T-B4-7：磁盘后缀不再恒 .png，写死 image/png 会让
  // jpeg/webp 的历史行在这里集体变成碎图）
  useEffect(() => {
    for (const it of items) {
      if (!it.file || thumbs[it.id] !== undefined) continue;
      screenshotHistoryGet(it.id)
        .then((d) =>
          setThumbs((prev) => ({
            ...prev,
            [it.id]: `data:${d.format};base64,${d.png_b64}`,
          })),
        )
        .catch(() => setThumbs((prev) => ({ ...prev, [it.id]: null })));
    }
  }, [items, thumbs]);

  // 配置真源读一次（T-B4-8）：既给预设区高亮"当前是哪枚"，也是写回时的展开基底
  useEffect(() => {
    if (!IN_TAURI) return;
    hostConfigGet("screenshot")
      .then((cfg) => {
        setShotCfg(cfg);
        setPostActions(readPostActions(cfg));
      })
      .catch((e) =>
        reportError(e, { context: "截图设置读取失败", dedupeKey: "shot-settings" }),
      );
  }, []);

  const reloadTargets = useCallback(() => {
    screenshotUploadTargets()
      .then(setTargets)
      .catch((e) =>
        reportError(e, {
          context: "上传目标读取失败",
          dedupeKey: "shot-upload-targets",
          toast: false,
        }),
      );
  }, []);

  // 目标表随面板打开读一次（T-B4-9）：纯读，一个字节都不往外发
  useEffect(() => {
    if (!IN_TAURI) return;
    reloadTargets();
  }, [reloadTargets]);

  /**
   * 配置被模块拒收 / 链式上传失败，都只存在于事件总线上（写盘本身是成功的）。
   * 不接这两个事件，用户看到的就只是"我点了保存，什么都没发生"。
   */
  useEffect(() => {
    if (!IN_TAURI) return;
    let unlisten: (() => void) | null = null;
    let disposed = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen<{ topic: string; payload: Record<string, unknown> }>("nf:event", (e) => {
          const { topic, payload } = e.payload;
          if (topic === "host.config_rejected" && payload.module === "screenshot") {
            setCfgRejected(String(payload.error ?? ""));
          } else if (topic === "screenshot.upload_failed") {
            notify("error", "上传失败（截图本身已完成）", String(payload.message ?? ""));
          }
        }),
      )
      .then((u) => {
        if (disposed) u();
        else unlisten = u;
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const applyPreset = useCallback(
    async (preset: { label: string; actions: string[] }) => {
      // 没读到配置就写 = 拿 {} 覆掉用户全部截图设置（host_config_set 是整份替换语义）：
      // 宁可不响应这一次点击
      if (!shotCfg) return;
      setPresetBusy(preset.label);
      try {
        const merged = { ...shotCfg, post_actions: preset.actions };
        await hostConfigSet("screenshot", merged);
        setShotCfg(merged);
        setPostActions(preset.actions);
        notify("success", `完成后动作链：${preset.label}`, "覆盖层「完成」钮下一次截图即按此执行");
      } catch (e) {
        notify("error", "保存动作链失败", parseAppError(e)?.data.message ?? String(e));
      } finally {
        setPresetBusy(null);
      }
    },
    [shotCfg],
  );

  /**
   * 上传目标读数（T-B4-9）。`enabled` 来自后端注册表而不是配置位——端点坏成不可用时
   * 注册表里就没有启用中的目标，面板跟着配置位显示"已启用"就是在骗用户。
   */
  const enabledTarget = useMemo(
    () => targets.find((t) => t.enabled) ?? null,
    [targets],
  );
  const uploadHeaderName = cfgStr(shotCfg, "upload_header_name").trim();
  const uploadEnabled = shotCfg?.upload_enabled === true;
  const uploadTargetId = cfgStr(shotCfg, "upload_target");
  /** 输入框没被碰过时跟随盘上的值；碰过之后编辑权归用户（半截 URL 也要看得见自己打了什么） */
  const endpointValue = endpointDraft ?? cfgStr(shotCfg, "upload_endpoint");

  /**
   * 整份展开写回（与 applyPreset 同一条纪律）：`host_config_set` 是替换语义，
   * 配置快照没读到手时宁可不响应这次点击。
   */
  const writeCfg = useCallback(
    async (patch: Record<string, unknown>) => {
      if (!shotCfg) return false;
      try {
        const merged = { ...shotCfg, ...patch };
        await hostConfigSet("screenshot", merged);
        setShotCfg(merged);
        // 上一轮的拒收话术不该顶着这一轮已改对的配置继续挂在屏幕上
        setCfgRejected(null);
        reloadTargets();
        return true;
      } catch (e) {
        notify("error", "保存上传设置失败", parseAppError(e)?.data.message ?? String(e));
        return false;
      }
    },
    [shotCfg, reloadTargets],
  );

  const saveEndpoint = useCallback(async () => {
    // 没碰过输入框就没有"新值"要存——在这种档上写盘等于把用户的端点清空
    if (endpointDraft === null) return;
    const value = endpointDraft.trim();
    setEndpointDraft(null);
    if (value === cfgStr(shotCfg, "upload_endpoint")) return;
    await writeCfg({ upload_endpoint: value });
  }, [endpointDraft, shotCfg, writeCfg]);

  const uploadShot = useCallback(
    async (id: string) => {
      setUploadBusy(id);
      try {
        // 端点要求请求头时，凭据值只进这一次 invoke；不需要头就明确传 null
        const link = await screenshotUpload(id, uploadHeaderName ? credValue : null);
        setCredValue("");
        setLastLink(link);
        notify("success", "已上传", link);
      } catch (e) {
        notify("error", "上传失败", parseAppError(e)?.data.message ?? String(e));
      } finally {
        setUploadBusy(null);
      }
    },
    [uploadHeaderName, credValue],
  );  const saveAs = useCallback(async (id: string, file: string | null, fmt: ExportFormat) => {
    try {
      const name = await saveShotAs(id, file, fmt);
      notify("success", "已交给浏览器下载", `${name} · 落点为浏览器下载目录（不是设置里的保存目录）`);
    } catch (e) {
      reportError(e, { context: "另存为失败", dedupeKey: `shot-saveas-${id}` });
    }
  }, []);

  const startCapture = useCallback(() => {
    void import("../../windows/overlayController")
      .then((m) => m.startOverlay("shot"))
      .catch((e) => reportError(e, { context: "发起截取失败", dedupeKey: "shot-start" }));
  }, []);

  /** 菜单展开时现读一次窗口表（每次展开都重读：窗口随时在开合，缓存即撒谎） */
  const reloadWindows = useCallback(() => {
    setWinsErr(null);
    screenshotWindows()
      .then((list) => setWins(list))
      .catch((e) => {
        // 失败不复用上一轮列表：拿旧数据把菜单填满，用户会以为那批窗口还开着
        setWins(null);
        setWinsErr(parseAppError(e)?.data.message ?? String(e));
      });
  }, []);

  const startWindowShot = useCallback((hwnd: number) => {
    void import("../../windows/overlayController")
      .then((m) => m.startOverlay("shot", hwnd))
      .catch((e) =>
        reportError(e, { context: "发起窗口截取失败", dedupeKey: "shot-start-window" }),
      );
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

  // 最小化窗不进下拉（选了也是被宿主明说拒绝），但它们**留在表里**计数：
  // 空表文案要说清是"一个窗口都没有"还是"有三个都最小化了"，后者用户自己就能动手
  const pickable = useMemo(() => (wins ?? []).filter((w) => !w.minimized), [wins]);
  const minimized = (wins ?? []).length - pickable.length;

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
        <Menu
          open={winsOpen}
          onOpenChange={(_, d) => {
            setWinsOpen(d.open);
            if (d.open) reloadWindows();
          }}
        >
          <MenuTrigger disableButtonEnhancement>
            <MenuButton size="small">截取窗口</MenuButton>
          </MenuTrigger>
          <MenuPopover>
            <MenuList>
              {winsErr ? (
                <MenuItem disabled>窗口枚举失败：{winsErr}</MenuItem>
              ) : wins === null ? (
                <MenuItem disabled>正在枚举窗口…</MenuItem>
              ) : pickable.length === 0 ? (
                // 红线：空表要说话，不能给一个点开啥也没有的空框（静默无反应＝用户只会反复点）
                <MenuItem disabled>
                  {minimized > 0
                    ? `没有可截取的窗口（${minimized} 个窗口已最小化，请先恢复）`
                    : "没有可截取的窗口"}
                </MenuItem>
              ) : (
                pickable.map((w) => (
                  <MenuItem key={w.hwnd} onClick={() => startWindowShot(w.hwnd)}>
                    {w.title} · {w.width}×{w.height}
                  </MenuItem>
                ))
              )}
            </MenuList>
          </MenuPopover>
        </Menu>
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
        {/* key 带上目标行：换行时整块重挂，上一行的预览不会顶着新图的脸留在屏幕上 */}
        {beautify && (
          <BeautifyPopover
            key={`${beautify.id}:${beautify.label}`}
            target={beautify}
            onClose={() => setBeautify(null)}
          />
        )}
        <Text className={styles.sectionTitle}>截图设置 · 完成后动作链</Text>
        <div className={styles.chipRow}>
          {POST_ACTION_PRESETS.map((p) => {
            const active = p.actions.join(",") === postActions.join(",");
            return (
              <Button
                key={p.label}
                size="small"
                appearance={active ? "primary" : "secondary"}
                aria-pressed={active}
                disabled={!shotCfg || presetBusy !== null}
                onClick={() => void applyPreset(p)}
              >
                {presetBusy === p.label ? "保存中…" : p.label}
              </Button>
            );
          })}
          <Badge appearance="outline" size="small">
            {postActions.length > 0 ? `当前：${postActions.join(" → ")}` : "当前：沿用「完成后…」三开关"}
          </Badge>
        </div>
        <Text className={styles.hint}>
          名单非空时，设置中心里「完成后复制 / 完成后保存 / 完成后贴图」三个开关不再参与动作链（它们只在名单为空时决定）；覆盖层上当面点的复制 / 保存 / 贴图钮永远以那一次点击为准。
        </Text>
        <Text className={styles.sectionTitle}>截图设置 · 上传</Text>
        <div className={styles.uploadRow}>
          <Switch
            label="启用上传目标"
            checked={uploadEnabled}
            disabled={!shotCfg || targets.length === 0}
            title={
              targets.length === 0
                ? "还没有可用的上传目标：先在下面填写端点"
                : "关闭时动作链里的「上传」直接跳过，一个字节都不发出去"
            }
            onChange={(_, d) => {
              const patch: Record<string, unknown> = { upload_enabled: d.checked };
              // 只注册了一枚目标而配置里还没选：勾选即选定。否则这颗开关点下去
              // 后端只会 warn 一句"未知的上传目标"，屏幕上什么也不会变
              if (d.checked && uploadTargetId === "" && targets.length === 1) {
                patch.upload_target = targets[0].id;
              }
              void writeCfg(patch);
            }}
          />
          <Badge appearance="outline" size="small">
            {enabledTarget
              ? `启用中：${enabledTarget.label}`
              : uploadEnabled
                ? "已勾选但目标未选定（点下方目标名选定）"
                : "未启用"}
          </Badge>
        </div>
        <div className={styles.chipRow}>
          {targets.map((t) => {
            const active = cfgStr(shotCfg, "upload_target") === t.id;
            return (
              <Button
                key={t.id}
                size="small"
                appearance={active && t.enabled ? "primary" : "secondary"}
                aria-pressed={active}
                disabled={!shotCfg}
                onClick={() => void writeCfg({ upload_target: t.id })}
              >
                {t.label} · {t.endpoint_display}
              </Button>
            );
          })}
          {targets.length === 0 && (
            // 空注册表要说清成因，否则看起来像功能被删了：唯一的可能是端点还没填
            <Text className={styles.hint}>先填端点，目标表才会有内容</Text>
          )}
        </div>
        <div className={styles.uploadRow}>
          <Input
            size="small"
            style={{ width: "360px" }}
            placeholder="上传端点（仅 https，或本机 http://127.0.0.1 · http://localhost）"
            value={endpointValue}
            onChange={(_, d) => setEndpointDraft(d.value)}
          />
          <Button
            size="small"
            disabled={!shotCfg || endpointDraft === null}
            onClick={() => void saveEndpoint()}
          >
            保存端点
          </Button>
          {/* 凭据值：这一格输入框是它唯一的容身处，写配置的那几条通路里没有它 */}
          {uploadHeaderName && (
            <Input
              size="small"
              type="password"
              style={{ width: "220px" }}
              placeholder={`${uploadHeaderName} 的值（仅本次调用，不落盘）`}
              value={credValue}
              onChange={(_, d) => setCredValue(d.value)}
            />
          )}
        </div>
        <InlineError text={cfgRejected} />
        {lastLink && (
          <div className={styles.uploadRow}>
            <Text className={styles.hint}>最近一次直链：{lastLink}</Text>
            <Button size="small" appearance="subtle" onClick={() => copyText(lastLink)}>
              复制直链
            </Button>
          </div>
        )}
        <Text className={styles.hint}>
          动作链里的「上传」用的就是这里勾选的目标；端点要求请求头时，链式通路拿不到凭据值（值只活在面板这一格），那种端点请改用历史行的上传钮。明文 http 只允许本机，写往公网会被拒。
        </Text>
        <div className={styles.chipRow}>
          <DeferredBadge label="WebDAV 上传目标" decisionRef="D-29 §9.1-⑩" />
        </div>
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
            {visible.map((it) => {
              const thumb = thumbs[it.id];
              return (
              <div key={it.id} className={styles.card}>
                {thumb ? (
                  <img className={styles.thumb} src={thumb} alt={`截图 ${it.width}x${it.height}`} />
                ) : (
                  <div className={styles.thumbMissing}>
                    {it.file ? (thumb === null ? "文件已丢失" : "加载缩略图…") : "未保存文件"}
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
                  {/* 上传钮只在"确有启用中的目标"时出现（T-B4-9）：默认关的通路不该
                      在每一行占一个位置，点了只会得到一句"没有启用中的上传目标"。
                      端点要求请求头而凭据值还没填时禁用——那一次点击注定失败。 */}
                  {enabledTarget && (
                    <Button
                      size="small"
                      disabled={
                        !it.file ||
                        uploadBusy !== null ||
                        (uploadHeaderName !== "" && credValue.trim() === "")
                      }
                      title={
                        uploadHeaderName && credValue.trim() === ""
                          ? `该端点需要「${uploadHeaderName}」请求头，请先在上方填入本次凭据值`
                          : `传到 ${enabledTarget.endpoint_display}`
                      }
                      onClick={() => void uploadShot(it.id)}
                    >
                      {uploadBusy === it.id ? "上传中…" : "上传"}
                    </Button>
                  )}
                  {/* 溢出菜单三枚（T-B4-7）：另存为哪一档由用户点出来，面板不替他猜。
                      无未保存文件的行整只菜单禁用——没有字节可下载时给钮就是假可点。 */}
                  <Menu>
                    <MenuTrigger disableButtonEnhancement>
                      <MenuButton size="small" appearance="subtle" disabled={!it.file}>
                        另存为
                      </MenuButton>
                    </MenuTrigger>
                    <MenuPopover>
                      <MenuList>
                        {EXPORT_FORMATS.map((f) => (
                          <MenuItem key={f} onClick={() => void saveAs(it.id, it.file, f)}>
                            {EXPORT_LABEL[f]}
                          </MenuItem>
                        ))}
                      </MenuList>
                    </MenuPopover>
                  </Menu>
                  {it.ocr_text && (
                    <Button size="small" onClick={() => copyText(it.ocr_text ?? "")}>
                      复制文字
                    </Button>
                  )}
                  {/* 美化两枚（T-B4-6）：入口只选定"导出带哪个动作"，真正落字节的是
                      BeautifyPopover 里那颗导出钮——菜单点下去不写盘，用户还能取消。 */}
                  <Menu>
                    <MenuTrigger disableButtonEnhancement>
                      <MenuButton size="small" appearance="subtle" disabled={!it.file}>
                        美化
                      </MenuButton>
                    </MenuTrigger>
                    <MenuPopover>
                      <MenuList>
                        <MenuItem onClick={() => setBeautify({ id: it.id, label: "美化另存", actions: ["save"] })}>
                          美化另存
                        </MenuItem>
                        <MenuItem onClick={() => setBeautify({ id: it.id, label: "美化复制", actions: ["copy"] })}>
                          美化复制
                        </MenuItem>
                      </MenuList>
                    </MenuPopover>
                  </Menu>
                </div>
              </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
