import { useCallback, useEffect, useState } from "react";
import {
  Button,
  makeStyles,
  Text,
  tokens,
  Tooltip,
} from "@fluentui/react-components";
import {
  clipboardSearch,
  clipboardSecretReveal,
  hostConfigGet,
  hostConfigSet,
  parseAppError,
  type ClipEntry,
} from "../../../ipc/client";
import { IN_TAURI } from "../../../ipc/env";
import { notify, reportError } from "../../../stores/notifications";
import { confirmAction } from "../../../stores/confirm";
import EmptyStateView from "../../../components/EmptyState";
import { fmtTime } from "../display";

/**
 * 敏感库子面板（T-B3-5，细案 01§2「敏感库」+ §7.1 掩码行不跳版）：
 * 列表恒为掩码（`clipboard_search` 的 preview 即遮蔽文案，明文不在响应里）；
 * 明文只经 `clipboard_secret_reveal` 单口出，出前二次确认、出后宿主写审计。
 * 「复制」走浏览器剪贴板 API 而非 clipboard_paste——粘贴口对敏感行照拒（004），
 * 拿它复制等于把拒语义绕一圈，故此处文案如实写"复制已揭示文本"。
 */

/** 类目标签与 clipboard-core `SECRET_CATEGORY_LABEL` 同字面（v1 不报具体密钥类型） */
const SECRET_CATEGORY = "敏感内容";

/**
 * 字面量转正则（T-B3-7「永不入库」）：把用户看到的原文变成只匹配它自身的模式。
 * 元字符表与 clipboard-core `query::to_literal_regex` 逐字符一致，两侧各有一枚测试
 * 断言同一份期望字面串——设置卡里显示的那条规则必须就是实际生效的那条。
 * 刻意不锚定：命中子串即算屏蔽，锚定会让"同一段内容换个前后缀再来一次"静默漏过。
 */
const REGEX_META = "\\^$.*+?()[]{}|";
export function toLiteralRegex(s: string): string {
  let out = "";
  for (const ch of s) out += REGEX_META.includes(ch) ? `\\${ch}` : ch;
  return out;
}

const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", flex: 1, minHeight: 0 },
  toolbar: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "0 20px",
    height: "40px",
    flexShrink: 0,
  },
  hint: {
    marginLeft: "auto",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  list: { flex: 1, overflowY: "auto", padding: "4px 20px 20px" },
  row: {
    padding: "10px 12px",
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusLarge,
  },
  meta: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    marginBottom: "6px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground2,
  },
  chip: {
    fontSize: tokens.fontSizeBase100,
    padding: "1px 7px",
    borderRadius: tokens.borderRadiusCircular,
    border: `1px solid ${tokens.colorPaletteDarkOrangeForeground1}`,
    color: tokens.colorPaletteDarkOrangeForeground1,
  },
  time: { marginLeft: "auto", fontSize: tokens.fontSizeBase100, color: tokens.colorNeutralForeground3 },
  /** 掩码态与明文态共用同一固定高度容器：切换只换内容不换盒高（§7.1 不跳版） */
  slot: {
    height: "52px",
    overflowY: "auto",
    padding: "6px 10px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: tokens.colorNeutralBackground2,
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
  },
  mask: { color: tokens.colorNeutralForeground3 },
  plain: { margin: 0, whiteSpace: "pre-wrap", wordBreak: "break-all" },
  ops: { display: "flex", gap: "6px", marginTop: "8px" },
});

export default function SecretSection() {
  const styles = useStyles();
  const [rows, setRows] = useState<ClipEntry[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [revealed, setRevealed] = useState<Record<string, string>>({});
  const [busyId, setBusyId] = useState<string | null>(null);

  const refresh = useCallback(() => {
    if (!IN_TAURI) return;
    clipboardSearch({ group: "secret", page: 0, size: 100 })
      .then((p) => {
        setRows(p.items);
        // 列表一旦重拉，已揭示明文即收回：明文不跨刷新驻留
        setRevealed({});
      })
      .catch((e) =>
        reportError(e, { context: "敏感库读取失败", dedupeKey: "clip-secret-list", toast: false }),
      )
      .finally(() => setLoaded(true));
  }, []);

  useEffect(refresh, [refresh]);

  useEffect(() => {
    if (!IN_TAURI) return;
    let unlisten: (() => void) | null = null;
    let disposed = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("nf:event", (e) => {
          const topic = (e.payload as { topic?: string }).topic;
          if (topic === "clipboard.captured" || topic === "clipboard.deleted") refresh();
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
  }, [refresh]);

  const doReveal = async (e: ClipEntry) => {
    const current = revealed[e.id];
    if (current !== undefined) {
      // 收起即把明文从 DOM 里拿掉，不等刷新
      setRevealed((m) => {
        const next = { ...m };
        delete next[e.id];
        return next;
      });
      return;
    }
    const ok = await confirmAction({
      danger: true,
      title: `揭示${SECRET_CATEGORY}`,
      impact: [
        `明文将显示在本页，并在宿主日志留一条审计记录`,
        `来源：${e.source_app || "未知"} · ${fmtTime(e.created_at)}`,
      ],
      detail: "揭示不会解锁粘贴：粘贴口对敏感条目照旧拒投；列表刷新即收回明文。",
      confirmLabel: "揭示",
      command: e.id,
    });
    if (!ok) return;
    setBusyId(e.id);
    try {
      const r = await clipboardSecretReveal(e.id);
      setRevealed((m) => ({ ...m, [r.id]: r.text }));
    } catch (err) {
      notify("error", "揭示失败", parseAppError(err)?.data.message ?? String(err));
    } finally {
      setBusyId(null);
    }
  };

  const doCopy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      notify("success", "已复制已揭示文本", "复制的是本次揭示出的明文（非粘贴口）");
    } catch {
      notify("error", "复制失败", "浏览器剪贴板不可用，请手动选中复制");
    }
  };

  /**
   * 「永不入库」（T-B3-7）：以**当前显示的那一行文本**生成字面屏蔽规则。
   * 本面板恒为掩码清单，显示的是 `[敏感内容] 已加密存储` 而不是明文——所以生成的规则
   * 屏蔽的是这段掩码文案，不是那条密钥本身。确认弹窗把这件事说白并指路设置卡手写正则：
   * 悄悄把明文写进 clipboard.json 才是真正要防的事，这里结构上就走不到那一步。
   */
  const doNeverStore = async (e: ClipEntry) => {
    const pattern = toLiteralRegex(e.preview);
    const ok = await confirmAction({
      danger: true,
      title: "永不入库：追加屏蔽规则",
      impact: [
        `将向「内容屏蔽规则」追加一条：${pattern}`,
        "此后命中该规则的复制内容不入库、也不发通知（仅文本捕获面）",
      ],
      detail:
        "本行显示的是掩码预览而非明文，故这条规则匹配的是掩码文案本身。要屏蔽原始内容，请到「统计与设置 → 内容屏蔽规则」手写正则（例：\\d{6}$）。",
      confirmLabel: "添加规则",
      command: e.id,
    });
    if (!ok) return;
    setBusyId(e.id);
    try {
      const cfg = await hostConfigGet("clipboard");
      const prev = Array.isArray(cfg.block_patterns)
        ? cfg.block_patterns.filter((x): x is string => typeof x === "string")
        : [];
      if (prev.includes(pattern)) {
        notify("info", "该规则已在屏蔽表中", pattern);
        return;
      }
      // host_config_set 是整份替换语义：先展开盘上全量再覆写单键，其余键不得丢
      await hostConfigSet("clipboard", { ...cfg, block_patterns: [...prev, pattern] });
      notify("success", "已加入内容屏蔽", `${pattern} 即时生效`);
    } catch (err) {
      notify("error", "保存屏蔽规则失败", parseAppError(err)?.data.message ?? String(err));
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className={styles.root}>
      <div className={styles.toolbar}>
        <Text className={styles.hint}>
          {`${SECRET_CATEGORY} ${rows.length} 条 · 命中密钥/Token 规则的内容以 AES-256-GCM 信封入库（D-04）`}
        </Text>
      </div>
      <div className={styles.list}>
        {rows.length === 0 && (
          <EmptyStateView
            text={`暂无${SECRET_CATEGORY}条目：复制内容命中密钥/卡号/身份证规则时自动加密入库，列表只显掩码。`}
            loading={!loaded}
          />
        )}
        {rows.map((e) => {
          const plain = revealed[e.id];
          return (
            <div key={e.id} className={styles.row}>
              <div className={styles.meta}>
                <span className={styles.chip}>{SECRET_CATEGORY}</span>
                <span>{e.source_app || "未知来源"}</span>
                <span className={styles.time}>{fmtTime(e.created_at)}</span>
              </div>
              <div className={styles.slot} aria-label="内容区">
                {plain === undefined ? (
                  <span className={styles.mask}>{e.preview}</span>
                ) : (
                  <pre className={styles.plain}>{plain}</pre>
                )}
              </div>
              <div className={styles.ops}>
                <Tooltip
                  content={plain === undefined ? "需二次确认，成功后写审计日志" : "收回明文"}
                  relationship="description"
                >
                  <Button
                    size="small"
                    appearance={plain === undefined ? "primary" : "subtle"}
                    disabled={busyId === e.id}
                    onClick={() => void doReveal(e)}
                  >
                    {plain === undefined ? "揭示" : "收起"}
                  </Button>
                </Tooltip>
                {plain !== undefined && (
                  <Button size="small" onClick={() => void doCopy(plain)}>
                    复制
                  </Button>
                )}
                <Tooltip
                  content="以本行显示的文本生成字面屏蔽规则，需二次确认"
                  relationship="description"
                >
                  <Button
                    size="small"
                    disabled={busyId === e.id}
                    onClick={() => void doNeverStore(e)}
                  >
                    永不入库
                  </Button>
                </Tooltip>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
