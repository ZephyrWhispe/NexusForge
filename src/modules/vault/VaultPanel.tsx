import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Divider,
  Drawer,
  DrawerBody,
  DrawerFooter,
  DrawerHeader,
  DrawerHeaderTitle,
  Input,
  Select,
  SpinButton,
  Spinner,
} from "@fluentui/react-components";
import {
  PasswordRegular,
  LockClosedRegular,
  AddRegular,
  DeleteRegular,
  CopyRegular,
  DismissRegular,
  EditRegular,
  EyeRegular,
  EyeOffRegular,
  CheckmarkRegular,
} from "@fluentui/react-icons";
import {
  parseAppError,
  vaultChangeMasterPassword,
  vaultCopyPassword,
  vaultCreate,
  vaultEntries,
  vaultEntryAdd,
  vaultEntryDelete,
  vaultEntryUpdate,
  vaultFolderCreate,
  vaultFolderDelete,
  vaultFolderRename,
  vaultFolders,
  vaultGeneratePassword,
  vaultHelloDisable,
  vaultHelloEnable,
  vaultHelloUnlock,
  vaultLock,
  vaultNotifyBlur,
  vaultStatus,
  vaultTotpNow,
  vaultUnlock,
  type EntryFieldDto,
  type PasswordPolicyDto,
  type VaultEntryDto,
  type VaultFolderDto,
  type VaultStatusDto,
} from "../../ipc/client";
import type { PanelProps } from "../../layout/panels";
import { IN_TAURI } from "../../ipc/env";
import { notify, reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import { keyActivate } from "../../a11y";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";
import DataToolbar from "../../components/DataToolbar";
import ListFooter from "../../components/ListFooter";
import Section from "../../components/Section";
import { FORM_CARD_W, SHELL, SPACING, TIER_W } from "../../components/nfTiers";

/**
 * 密码库面板（docs/impl/05 V7，M5 v1）三态渲染：
 * ① uninitialized 建库（Argon2 64MiB，秒级）② locked 解锁（5 次错误 300s 冷却）
 * ③ unlocked 条目管理（文件夹 + 条目 + TOTP + 生成器）。vault.* 事件驱动刷新。
 *
 * D-43 C4 重排：三态共用同一副区块骨架（「保险库」＋「条目清单」两枚带标题的 Section，
 * 各挂锚点），区块次序不随状态塌陷——锚点式二级导航因此在锁定态也指得到真落点，
 * 而不是造一枚点了没反应的撒谎按钮。通栏带（DataToolbar/ListFooter）自留白、
 * 卡片进 stack 内缩 20px（00 规范 1 节：工具条随主体吸顶、页脚通栏）。
 */
const useStyles = makeStyles({
  root: {
    flex: 1,
    minWidth: 0,
    overflowY: "auto",
    display: "flex",
    flexDirection: "column",
    paddingBottom: SPACING.x24,
  },
  stack: {
    display: "flex",
    flexDirection: "column",
    gap: SPACING.x16,
    padding: `0 ${SHELL.contentPad}`,
  },
  card: {
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    padding: "24px 28px",
    backgroundColor: tokens.colorNeutralBackground1,
    display: "flex",
    flexDirection: "column",
    gap: "12px",
    width: FORM_CARD_W,
  },
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  // 抽屉/表单列：字段间 12（00 规范 2 节分组内），Divider 由组间承担分隔
  formStack: { display: "flex", flexDirection: "column", gap: SPACING.x12 },
  kv: {
    display: "grid",
    gridTemplateColumns: `${TIER_W.s} 1fr`,
    gap: "8px",
    alignItems: "center",
  },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  columns: {
    display: "grid",
    gridTemplateColumns: "200px 1fr",
    gap: "16px",
    flex: 1,
    minHeight: 0,
  },
  side: {
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground1,
    padding: "8px",
    display: "flex",
    flexDirection: "column",
    gap: "4px",
    alignSelf: "start",
  },
  folderItem: {
    padding: "6px 10px",
    borderRadius: tokens.borderRadiusMedium,
    cursor: "pointer",
    display: "flex",
    justifyContent: "space-between",
    alignItems: "center",
  },
  folderActive: {
    backgroundColor: tokens.colorBrandBackground2,
    color: tokens.colorBrandForeground1,
    fontWeight: tokens.fontWeightSemibold,
  },
  entryList: { display: "flex", flexDirection: "column", gap: "8px" },
  entry: {
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground1,
    padding: "10px 14px",
    display: "flex",
    flexDirection: "column",
    gap: "8px",
  },
  fieldRow: { display: "flex", alignItems: "center", gap: "8px" },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase300 },
  otp: {
    fontFamily: "Consolas, monospace",
    fontSize: tokens.fontSizeBase500,
    fontWeight: tokens.fontWeightSemibold,
    letterSpacing: "3px",
    color: tokens.colorBrandForeground1,
  },
  otpBar: { height: "3px", borderRadius: "2px", backgroundColor: tokens.colorNeutralStroke2 },
});

function errText(e: unknown): string {
  const app = parseAppError(e);
  return app ? `${app.data.code}: ${app.data.message}` : String(e);
}

// ---------------------------------------------------------------------------
// TOTP 徽章：本地倒计时 + 每 5s 或跨周期时重新取码
// ---------------------------------------------------------------------------

function TotpBadge({ secret }: { secret: string }) {
  const styles = useStyles();
  const [otp, setOtp] = useState("");
  const [remaining, setRemaining] = useState(0);
  useEffect(() => {
    let alive = true;
    let timer: number | undefined;
    // PERF-06：仅跨周期时向后端取码（30s 一次而非每秒 1 次 IPC）；
    // 秒级倒计时用本地时钟推算，零 IPC、每秒仅本组件一次 setState
    const fetchCode = async () => {
      try {
        const [code, rem] = await vaultTotpNow(secret);
        if (!alive) return;
        setOtp(code);
        scheduleCountdown(rem);
      } catch {
        if (alive) setOtp("------");
      }
    };
    const scheduleCountdown = (rem: number) => {
      const endAt = Date.now() + Math.max(1, rem) * 1000;
      const tickLocal = () => {
        if (!alive) return;
        const left = Math.max(0, Math.round((endAt - Date.now()) / 1000));
        setRemaining(left);
        if (left <= 0) {
          void fetchCode();
        } else {
          timer = window.setTimeout(tickLocal, 1000);
        }
      };
      tickLocal();
    };
    void fetchCode();
    return () => {
      alive = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [secret]);
  return (
    <span className={styles.fieldRow}>
      <span className={styles.otp}>{otp}</span>
      <span className={styles.muted}>{remaining}s</span>
    </span>
  );
}

// ---------------------------------------------------------------------------
// 字段值渲染：password 打码 + 显示/复制；totp 类型进度条
// ---------------------------------------------------------------------------

function FieldValue({ field, entryId }: { field: EntryFieldDto; entryId: string }) {
  const styles = useStyles();
  const [shown, setShown] = useState(false);
  // COR-18 自保：字段值变化即复位打码态（key 已按条目+字段稳定，这里是双保险）
  useEffect(() => {
    setShown(false);
  }, [field.value, field.key]);
  // 密码字段走后端 vault_copy_password：write_back（D-10 回写窗口，防自捕获）
  // + 到期条件清除（D-24）；非敏感字段直接浏览器剪贴板
  const copy = () => {
    if (field.kind === "password" && IN_TAURI) {
      vaultCopyPassword(entryId, field.key).catch((e) =>
        reportError(e, { context: "复制密码失败" }),
      );
    } else {
      void navigator.clipboard.writeText(field.value).catch((e) => reportError(e, { context: "复制到剪贴板失败" }));
    }
  };
  if (field.kind === "otp") return <TotpBadge secret={field.value} />;
  const masked = field.kind === "password" && !shown;
  return (
    <span className={styles.fieldRow}>
      {field.key && <span className={styles.muted}>{field.key}</span>}
      <span className={styles.mono}>{masked ? "••••••••" : field.value}</span>
      {field.kind === "password" && (
        <Button
          appearance="subtle"
          size="small"
          icon={shown ? <EyeOffRegular /> : <EyeRegular />}
          onClick={() => setShown((v) => !v)}
          title={shown ? "隐藏" : "显示"}
        />
      )}
      <Button
        appearance="subtle"
        size="small"
        icon={<CopyRegular />}
        onClick={copy}
        title={field.kind === "password" ? "复制（到期后若仍是此密码将自动清除）" : "复制"}
      />
    </span>
  );
}

// ---------------------------------------------------------------------------
// 条目编辑对话框（新建 / 编辑共用）
// ---------------------------------------------------------------------------

interface EditorState {
  entry: VaultEntryDto | null; // null = 新建
  title: string;
  favorite: boolean;
  totpSecret: string;
  fields: EntryFieldDto[];
  // 生成器
  genLength: number;
  genUpper: boolean;
  genLower: boolean;
  genDigits: boolean;
  genSymbols: boolean;
  genAmbiguous: boolean;
  /** 打开时表单内容的序列化快照（脏检测基线，D-18：关闭丢失需确认） */
  initial: string;
}

/** 编辑对话框内容 = 生成器参数以外的部分（生成器只是填值工具，不算表单内容） */
export function serializeEditorForm(e: {
  entry: VaultEntryDto | null;
  title: string;
  favorite: boolean;
  totpSecret: string;
  fields: EntryFieldDto[];
}): string {
  return JSON.stringify({
    entry: e.entry?.id ?? null,
    title: e.title,
    favorite: e.favorite,
    totpSecret: e.totpSecret,
    fields: e.fields,
  });
}

/** 打开时的空态/原值快照与当前内容不一致即为脏（纯函数，随 D-18 回归测试） */
export function isEditorDirty(editor: EditorState): boolean {
  return serializeEditorForm(editor) !== editor.initial;
}

function emptyEditor(): EditorState {
  const base = {
    entry: null,
    title: "",
    favorite: false,
    totpSecret: "",
    fields: [{ key: "password", kind: "password", value: "" }] as EntryFieldDto[],
    genLength: 16,
    genUpper: true,
    genLower: true,
    genDigits: true,
    genSymbols: true,
    genAmbiguous: false,
  };
  return { ...base, initial: serializeEditorForm(base) };
}

const KIND_LABELS: Record<EntryFieldDto["kind"], string> = {
  password: "密码",
  url: "链接",
  note: "备注",
  otp: "动态码密钥",
  text: "文本",
};

/** 生成器长度档：下界 4 来自后端"长度不得小于所选字符类数"（全开＝4 类），上界为界面档、后端无上限 */
const GEN_LEN = { min: 4, max: 128 } as const;

function editorFromEntry(entry: VaultEntryDto): EditorState {
  const base = {
    entry,
    title: entry.title,
    favorite: entry.favorite,
    totpSecret: entry.totp_secret ?? "",
    fields: entry.fields.map((f) => ({ ...f })),
    genLength: 16,
    genUpper: true,
    genLower: true,
    genDigits: true,
    genSymbols: true,
    genAmbiguous: false,
  };
  return { ...base, initial: serializeEditorForm(base) };
}

export default function VaultPanel({ search }: PanelProps) {
  const styles = useStyles();
  const [status, setStatus] = useState<VaultStatusDto | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [formErr, setFormErr] = useState<string | null>(null);

  // 建库/解锁表单
  const [pw, setPw] = useState("");
  const [pwConfirm, setPwConfirm] = useState("");
  const [lockoutLeft, setLockoutLeft] = useState(0);

  // 改主密码对话框（T-B1-3；红线：change_master_password 不经 unlock()，
  // 错旧密不递增尝试计数——文案不得宣称锁定保护，错误原样内联展示）
  const [mpOpen, setMpOpen] = useState(false);
  const [mpOld, setMpOld] = useState("");
  const [mpNew, setMpNew] = useState("");
  const [mpConfirm, setMpConfirm] = useState("");
  const [mpErr, setMpErr] = useState<string | null>(null);

  // 解锁态数据
  const [folders, setFolders] = useState<VaultFolderDto[]>([]);
  const [activeFolder, setActiveFolder] = useState<string | "all">("all");
  const [entries, setEntries] = useState<VaultEntryDto[]>([]);
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [newFolderName, setNewFolderName] = useState("");
  // 文件夹行内改名（T-B1-3）：null = 无进行中；否则 {文件夹 id, 草稿名}
  const [renaming, setRenaming] = useState<{ id: string; name: string } | null>(null);
  // callback ref：进入改名态挂载即聚焦（jsx-a11y 禁 autoFocus，attach 时序等价）
  const focusRename = useCallback((el: HTMLInputElement | null) => el?.focus(), []);
  const [loaded, setLoaded] = useState(false);
  const reloadRef = useRef<() => void>(() => undefined);

  const refresh = useCallback(async () => {
    try {
      const s = await vaultStatus();
      setStatus(s);
      setLockoutLeft(s.lockout_remaining_secs);
      if (s.state === "unlocked") {
        const [f, e] = await Promise.all([
          vaultFolders(),
          // 搜索走后端（vault_entries search 参数，仅匹配标题 model.rs LIKE）；
          // 空白查询传 null = 不参与过滤
          vaultEntries(activeFolder === "all" ? null : activeFolder, search.trim() || null),
        ]);
        setFolders(f);
        setEntries(e);
      }
    } catch (err) {
      setLoadErr(errText(err));
    } finally {
      setLoaded(true);
    }
  }, [activeFolder, search]);
  reloadRef.current = () => void refresh();

  useEffect(() => {
    reloadRef.current();
  }, [activeFolder, search]);

  // 冷却倒计时（锁定态每秒递减）
  useEffect(() => {
    if (status?.state !== "locked" || lockoutLeft <= 0) return;
    const t = setInterval(() => setLockoutLeft((v) => (v > 0 ? v - 1 : 0)), 1000);
    return () => clearInterval(t);
  }, [status?.state, lockoutLeft]);

  // vault.* 事件驱动刷新 + V5 自动锁定预警（D-24）
  useEffect(() => {
    if (!IN_TAURI) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false; // COR-17：卸载早于 listen resolve 时迟到监听器立即注销
    import("@tauri-apps/api/event").then(({ listen }) =>
      listen("nf:event", (e) => {
        const p = e.payload as { topic?: string; payload?: { lock_in_secs?: number } };
        if (p.topic === "vault.state_changed" || p.topic === "vault.entries_changed")
          reloadRef.current();
        if (p.topic === "vault.auto_lock_warning")
          notify(
            "warn",
            "密码库即将自动锁定",
            `${p.payload?.lock_in_secs ?? 30}s 后锁定（空闲/失焦超限）`,
          );
      }),
    ).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // V5 失焦线信号（D-24）：窗口 blur/focus 上报后端计时
  useEffect(() => {
    if (!IN_TAURI) return;
    const onBlur = () => void vaultNotifyBlur(true).catch(() => undefined);
    const onFocus = () => void vaultNotifyBlur(false).catch(() => undefined);
    window.addEventListener("blur", onBlur);
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("focus", onFocus);
    };
  }, []);

  // ---- 动作 ----

  const doCreate = async () => {
    setFormErr(null);
    if (pw.length < 8) return setFormErr("主密码至少 8 个字符");
    if (pw !== pwConfirm) return setFormErr("两次输入不一致");
    setBusy(true);
    try {
      await vaultCreate(pw);
      setPw("");
      setPwConfirm("");
      await refresh();
    } catch (e) {
      setFormErr(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const doUnlock = async () => {
    setFormErr(null);
    setBusy(true);
    try {
      await vaultUnlock(pw);
      setPw("");
      await refresh();
    } catch (e) {
      setFormErr(errText(e));
      await refresh(); // 刷新冷却剩余
    } finally {
      setBusy(false);
    }
  };

  const doLock = async () => {
    await vaultLock().catch((e) => reportError(e, { context: "锁定密码库失败" }));
    setEntries([]);
    await refresh();
  };

  // Windows Hello 免密解锁（D-24）：失败后 refresh 以刷新熔断/冷却态
  const doHelloUnlock = async () => {
    setFormErr(null);
    setBusy(true);
    try {
      await vaultHelloUnlock();
      await refresh();
    } catch (e) {
      setFormErr(errText(e));
      await refresh();
    } finally {
      setBusy(false);
    }
  };

  const doHelloToggle = async () => {
    if (!status) return;
    setBusy(true);
    try {
      if (status.hello_enabled) {
        await vaultHelloDisable();
        notify("info", "已关闭免密解锁", "此后仅可用主密码解锁。");
      } else {
        await vaultHelloEnable();
        notify("success", "已启用免密解锁", "Windows Hello 与本机账户域绑定，连续失败 5 次自动熔断。");
      }
      await refresh();
    } catch (e) {
      reportError(e, { context: "切换免密解锁失败" });
    } finally {
      setBusy(false);
    }
  };

  const doSaveEntry = async () => {
    if (!editor) return;
    setFormErr(null);
    if (!editor.title.trim()) return setFormErr("标题不能为空");
    setBusy(true);
    try {
      const totp = editor.totpSecret.trim() || null;
      if (editor.entry) {
        await vaultEntryUpdate({
          ...editor.entry,
          title: editor.title.trim(),
          favorite: editor.favorite,
          totp_secret: totp,
          fields: editor.fields.filter((f) => f.key.trim() || f.value.trim()),
        });
      } else {
        await vaultEntryAdd({
          title: editor.title.trim(),
          folderId: activeFolder === "all" ? null : activeFolder,
          favorite: editor.favorite,
          totpSecret: totp,
          fields: editor.fields.filter((f) => f.key.trim() || f.value.trim()),
        });
      }
      setEditor(null);
      await refresh();
    } catch (e) {
      setFormErr(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const doDeleteEntry = async (entry: VaultEntryDto) => {
    if (
      !(await confirmAction({
        title: "删除凭据条目",
        impact: [
          `「${entry.title}」及其 ${entry.fields.length} 个加密字段将永久删除`,
          ...(entry.totp_secret ? ["TOTP 动态码密钥一并删除"] : []),
        ],
        detail: "密码库无回收站，删除后不可恢复。",
        confirmLabel: "删除",
      }))
    )
      return;
    try {
      await vaultEntryDelete(entry.id);
    } catch (e) {
      reportError(e, { context: "删除条目失败" });
    }
    await refresh();
  };

  /** 文件夹删除影响面：DTO 不含条目数，现拉取该夹条目计数（D-18 要求展示影响面） */
  const doDeleteFolder = async (f: VaultFolderDto) => {
    let count: number | null = null;
    try {
      count = (await vaultEntries(f.id, null)).length;
    } catch (e) {
      reportError(e, { context: "文件夹条目统计失败", dedupeKey: "vault-folder-count" });
    }
    if (
      !(await confirmAction({
        title: "删除文件夹",
        impact: [
          `将删除文件夹「${f.name}」`,
          count === null
            ? "其中条目将移至「全部条目」（条目保留）"
            : `其中 ${count} 个条目将移至「全部条目」（条目保留）`,
        ],
        confirmLabel: "删除文件夹",
      }))
    )
      return;
    await vaultFolderDelete(f.id).catch((e) => reportError(e, { context: "删除文件夹失败" }));
    if (activeFolder === f.id) setActiveFolder("all");
    await refresh();
  };

  /** 关闭编辑对话框：表单有未保存修改时经 ConfirmDialog 确认丢弃（D-18） */
  const closeEditor = async () => {
    if (!editor) return;
    if (
      isEditorDirty(editor) &&
      !(await confirmAction({
        title: editor.entry ? "放弃未保存的修改" : "放弃新建条目",
        impact: [editor.title.trim() ? `「${editor.title.trim()}」` : "（未命名条目）"],
        detail: "当前表单内容尚未保存，关闭后将丢失。",
        confirmLabel: "放弃并关闭",
      }))
    )
      return;
    setEditor(null);
  };

  const doAddFolder = async () => {
    const name = newFolderName.trim();
    if (!name) return;
    setNewFolderName("");
    await vaultFolderCreate(name).catch((e) => reportError(e, { context: "新建文件夹失败" }));
    await refresh();
  };

  // T-B1-3 行内改名：vault_folder_rename 对不存在的 id 返回 false（非报错），如实区分
  const doRenameFolder = async (f: VaultFolderDto) => {
    if (!renaming) return;
    const name = renaming.name.trim();
    if (!name || name === f.name) {
      setRenaming(null);
      return;
    }
    try {
      const ok = await vaultFolderRename(f.id, name);
      if (!ok) notify("warn", "未找到该文件夹", "可能被其他窗口删除，已刷新列表");
      await refresh();
    } catch (e) {
      reportError(e, { context: "重命名文件夹失败" });
    } finally {
      setRenaming(null);
    }
  };

  // T-B1-3 改主密码（红线）：错误经 InlineError 原样内联（VAULT_UNLOCK_001 = 旧密错误），
  // 不宣称尝试计数/冷却保护（该命令不经 unlock()，无锁定副作用）
  const doChangePassword = async () => {
    setMpErr(null);
    if (mpNew.length < 8) return setMpErr("新主密码至少 8 个字符");
    if (mpNew !== mpConfirm) return setMpErr("两次输入的新密码不一致");
    if (!mpOld) return setMpErr("请输入当前主密码");
    setBusy(true);
    try {
      await vaultChangeMasterPassword(mpOld, mpNew);
      setMpOld("");
      setMpNew("");
      setMpConfirm("");
      setMpOpen(false);
      notify("success", "主密码已修改", "DEK 已用新密码重包裹，条目数据未改动。");
    } catch (e) {
      setMpErr(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const doGen = async () => {
    if (!editor) return;
    setFormErr(null);
    const policy: PasswordPolicyDto = {
      length: editor.genLength,
      upper: editor.genUpper,
      lower: editor.genLower,
      digits: editor.genDigits,
      symbols: editor.genSymbols,
      avoid_ambiguous: editor.genAmbiguous,
    };
    try {
      const pw2 = await vaultGeneratePassword(policy);
      setEditor((s) => s && { ...s, fields: s.fields.map((f) => (f.kind === "password" && f.key === "password" ? { ...f, value: pw2 } : f)) });
    } catch (e) {
      setFormErr(errText(e));
    }
  };

  // ---- 渲染 ----

  if (!status) {
    return (
      <div className={styles.root}>
        {loadErr ? <InlineError text={loadErr} /> : <Spinner label="加载密码库状态…" />}
      </div>
    );
  }

  // 建库/解锁两态的表单进「保险库」区块（D-43 C4：三态共用同一副区块骨架，
  // 锚点式二级导航因此在任何状态下都指得到真落点，不造点了没反应的撒谎按钮）
  const initForm = (
    <div className={styles.card}>
      <Text className={styles.muted}>
        主密码经 Argon2id（64MiB 内存参数）派生密钥，用于加密全部条目。创建约需数秒。
      </Text>
      <Input
        type="password"
        aria-label="主密码"
        placeholder="主密码（≥8 位）"
        value={pw}
        onChange={(_, d) => setPw(d.value)}
      />
      <Input
        type="password"
        aria-label="确认主密码"
        placeholder="确认主密码"
        value={pwConfirm}
        onChange={(_, d) => setPwConfirm(d.value)}
      />
      <InlineError text={formErr} />
      <Button appearance="primary" disabled={busy} onClick={() => void doCreate()}>
        {busy ? "正在创建…" : "创建保险库"}
      </Button>
    </div>
  );

  const lockForm = (
    <div className={styles.card}>
      <Text className={styles.muted}>输入主密码解锁。连续错误 5 次将进入 300 秒冷却。</Text>
      <Input
        type="password"
        aria-label="主密码"
        placeholder="主密码"
        value={pw}
        onChange={(_, d) => setPw(d.value)}
        onKeyDown={(e) => e.key === "Enter" && lockoutLeft === 0 && !busy && void doUnlock()}
      />
      {lockoutLeft > 0 && (
        <Badge appearance="filled" color="danger">
          尝试次数过多，{lockoutLeft}s 后可重试
        </Badge>
      )}
      <InlineError text={formErr} />
      <Button
        appearance="primary"
        disabled={busy || lockoutLeft > 0}
        onClick={() => void doUnlock()}
      >
        {busy ? "正在解锁…" : "解锁"}
      </Button>
      {status.hello_available && status.hello_enabled && !status.hello_forced && (
        <Button
          icon={<PasswordRegular />}
          disabled={busy || lockoutLeft > 0}
          onClick={() => void doHelloUnlock()}
        >
          使用 Windows Hello 解锁
        </Button>
      )}
      {status.hello_forced && (
        <Text className={styles.muted}>
          免密校验连续失败过多，本次仅允许主密码解锁（主密码解锁成功后自动恢复）。
        </Text>
      )}
    </div>
  );

  // ---- 三态同构骨架（D-43 C4）----
  const unlocked = status.state === "unlocked";
  const header = status.kdf;
  const folderName = folders.find((f) => f.id === activeFolder)?.name ?? "已选文件夹";

  return (
    <div className={styles.root}>
      {unlocked && (
        // 00 规范 5 节：通栏工具条随主体吸顶，主按钮恒末位（「新建条目」是本页唯一造物的动作）
        <DataToolbar
          filters={
            activeFolder === "all" ? undefined : (
              <Button
                size="small"
                appearance="subtle"
                icon={<DismissRegular />}
                title="清除文件夹筛选"
                onClick={() => setActiveFolder("all")}
              >
                {`文件夹：${folderName}`}
              </Button>
            )
          }
          primary={
            <Button appearance="primary" icon={<AddRegular />} onClick={() => setEditor(emptyEditor())}>
              新建条目
            </Button>
          }
        />
      )}

      <div className={styles.stack}>
        <Section
          title={
            status.state === "uninitialized"
              ? "创建保险库"
              : unlocked
                ? "保险库与加密参数"
                : "解锁保险库"
          }
          anchor="vault.security"
          actions={
            unlocked ? (
              <>
                <Button icon={<LockClosedRegular />} onClick={() => void doLock()}>
                  立即锁定
                </Button>
                <Button
                  icon={<PasswordRegular />}
                  onClick={() => {
                    setMpErr(null);
                    setMpOpen(true);
                  }}
                >
                  修改主密码
                </Button>
                {status.hello_available && (
                  <Button
                    icon={<PasswordRegular />}
                    disabled={busy}
                    onClick={() => void doHelloToggle()}
                    title={
                      status.hello_enabled
                        ? "关闭后仅可用主密码解锁"
                        : "启用后可用 Windows Hello 免密解锁（本机账户域绑定）"
                    }
                  >
                    {status.hello_enabled ? "关闭免密解锁" : "启用免密解锁"}
                  </Button>
                )}
              </>
            ) : undefined
          }
        >
          {status.state === "uninitialized" && initForm}
          {status.state === "locked" && lockForm}
          {unlocked && (
            <div className={styles.kv}>
              <Text className={styles.muted}>库状态</Text>
              <div className={styles.row}>
                <Badge appearance="filled" color="informative">已解锁</Badge>
                {status.hello_forced && (
                  <Badge appearance="filled" color="warning">免密校验熔断中，本次仅主密码</Badge>
                )}
              </div>
              <Text className={styles.muted}>密钥派生</Text>
              {header ? (
                <span className={styles.mono}>
                  {`${header.kdf.algo} · 内存 ${header.kdf.m_cost_kib / 1024} MiB · 迭代 ${header.kdf.t_cost} · 并行 ${header.kdf.p_cost}`}
                </span>
              ) : (
                <Text className={styles.muted}>本次会话未取到库头部快照</Text>
              )}
              <Text className={styles.muted}>免密解锁</Text>
              <Text>
                {!status.hello_available
                  ? "本机 Windows Hello 不可用，仅主密码解锁"
                  : status.hello_enabled
                    ? "已启用（与本机账户域绑定，连续失败 5 次自动熔断）"
                    : "未启用"}
              </Text>
              <Text className={styles.muted}>条目加密</Text>
              <Text>字段级 AES-256-GCM；主密码不落盘，仅派生密钥</Text>
            </div>
          )}
        </Section>

        <Section title="条目清单" anchor="vault.entries">
          {unlocked ? (
            <div className={styles.columns}>
              <div className={styles.side}>
                <div
                  className={`${styles.folderItem} ${activeFolder === "all" ? styles.folderActive : ""}`}
                  onClick={() => setActiveFolder("all")}
                  role="button"
                  tabIndex={0}
                  onKeyDown={keyActivate(() => setActiveFolder("all"))}
                >
                  <Text>全部条目</Text>
                </div>
                {folders.map((f) => (
                  <div
                    key={f.id}
                    className={`${styles.folderItem} ${activeFolder === f.id ? styles.folderActive : ""}`}
                    onClick={() => setActiveFolder(f.id)}
                    role="button"
                    tabIndex={0}
                    onKeyDown={keyActivate(() => setActiveFolder(f.id))}
                  >
                    {renaming?.id === f.id ? (
                      // 行内改名（沿用新文件夹行的 Input+按钮形态，00-spec 控件档内）
                      <>
                        <Input
                          ref={focusRename}
                          size="small"
                          value={renaming.name}
                          onClick={(e) => e.stopPropagation()}
                          onChange={(_, d) => setRenaming({ id: f.id, name: d.value })}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") void doRenameFolder(f);
                            if (e.key === "Escape") setRenaming(null);
                          }}
                          style={{ minWidth: 0, flex: 1 }}
                        />
                        <Button
                          appearance="subtle"
                          size="small"
                          icon={<CheckmarkRegular />}
                          title="确认重命名"
                          onClick={(e) => {
                            e.stopPropagation();
                            void doRenameFolder(f);
                          }}
                        />
                      </>
                    ) : (
                      <>
                        <Text truncate>{f.name}</Text>
                        <Button
                          appearance="subtle"
                          size="small"
                          icon={<EditRegular />}
                          title="重命名文件夹"
                          onClick={(e) => {
                            e.stopPropagation();
                            setRenaming({ id: f.id, name: f.name });
                          }}
                        />
                        <Button
                          appearance="subtle"
                          size="small"
                          icon={<DeleteRegular />}
                          title="删除文件夹"
                          aria-label={`删除文件夹 ${f.name ?? ""}`.trim()}
                          onClick={(e) => {
                            e.stopPropagation();
                            void doDeleteFolder(f);
                          }}
                        />
                      </>
                    )}
                  </div>
                ))}
                <Divider />
                <div className={styles.row}>
                  <Input
                    size="small"
                    placeholder="新文件夹"
                    value={newFolderName}
                    onChange={(_, d) => setNewFolderName(d.value)}
                    onKeyDown={(e) => e.key === "Enter" && void doAddFolder()}
                  />
                  <Button
                  size="small"
                  icon={<AddRegular />}
                  aria-label="新建文件夹"
                  onClick={() => void doAddFolder()}
                />
                </div>
              </div>

              <div className={styles.entryList}>
                {loaded && entries.length === 0 && (
                  // D-18/00§4-4：后端已过滤，搜索无果 ≠ 空库两态如实分开
                  search.trim() ? (
                    <EmptyState text={`没有标题匹配「${search.trim()}」的条目（搜索仅按标题，由后端执行）。`} />
                  ) : (
                    <EmptyState text="暂无条目，点击「新建条目」添加。" />
                  )
                )}
                {entries.map((entry) => (
                  <div key={entry.id} className={styles.entry}>
                    <div className={styles.fieldRow}>
                      {entry.favorite && <Badge appearance="filled" color="brand">★</Badge>}
                      <Text weight="semibold" size={300}>
                        {entry.title}
                      </Text>
                      <span style={{ flex: 1 }} />
                      <Button
                        appearance="subtle"
                        size="small"
                        onClick={() => setEditor(editorFromEntry(entry))}
                      >
                        编辑
                      </Button>
                      <Button
                        appearance="subtle"
                        size="small"
                        icon={<DeleteRegular />}
                        aria-label={`删除条目 ${entry.title}`}
                        onClick={() => void doDeleteEntry(entry)}
                      />
                    </div>
                    {entry.fields.map((f) => (
                      // COR-18：业务键（条目 id + 字段名）替代位置索引——entries_changed/
                      // 解锁刷新后组件实例不跨数据复用，shown 明文态不得残留
                      <FieldValue key={`${entry.id}:${f.key}`} field={f} entryId={entry.id} />
                    ))}
                    {entry.totp_secret && <TotpBadge secret={entry.totp_secret} />}
                  </div>
                ))}
              </div>
            </div>
          ) : (
            <Text className={styles.muted}>
              {status.state === "uninitialized"
                ? "建库并解锁后，这里列出全部凭据条目。"
                : "解锁后这里列出全部凭据条目。"}
            </Text>
          )}
        </Section>
      </div>

      {unlocked && (
        <ListFooter
          left={`共 ${entries.length} 条`}
          right="字段以 AES-256-GCM 加密 · 搜索仅按标题，由后端过滤"
        />
      )}

      {editor && (
        // 条目编辑详情抽屉（D-43 C4 行为变更：由 480px Dialog 改右侧覆盖抽屉，面板档 03 §1
        // 把"全字段一滚到底的对话框"列为现状问题、§7.1 指定详情抽屉）。
        // OverlayDrawer 与 Dialog 同为焦点陷阱＋Esc＋遮罩关闭，所有关闭路径统一走
        // closeEditor()，未保存修改先经 ConfirmDialog 确认（D-18 判据不动）。
        // size 只有 320/592/940/full 四档，取 medium=592（档的 520 无对应档，不为它手写宽度）。
        <Drawer
          type="overlay"
          position="end"
          size="medium"
          open
          onOpenChange={() => void closeEditor()}
        >
          <DrawerHeader>
            <DrawerHeaderTitle>{editor.entry ? "编辑条目" : "新建条目"}</DrawerHeaderTitle>
          </DrawerHeader>
          <DrawerBody className={styles.formStack}>
            <Input
              placeholder="标题（如 GitHub）"
              value={editor.title}
              onChange={(_, d) => setEditor({ ...editor, title: d.value })}
            />
            <div className={styles.row}>
              <Checkbox
                label="收藏"
                checked={editor.favorite}
                onChange={(_, d) => setEditor({ ...editor, favorite: !!d.checked })}
              />
            </div>
            <Divider />
            {editor.fields.map((f, i) => (
              <div key={i} className={styles.row}>
                <Input
                  size="small"
                  style={{ width: TIER_W.s }}
                  placeholder="字段名"
                  value={f.key}
                  onChange={(_, d) =>
                    setEditor({
                      ...editor,
                      fields: editor.fields.map((x, j) => (j === i ? { ...x, key: d.value } : x)),
                    })
                  }
                />
                <Select
                  size="small"
                  style={{ width: TIER_W.s }}
                  value={f.kind}
                  onChange={(_, d) =>
                    setEditor({
                      ...editor,
                      fields: editor.fields.map((x, j) =>
                        j === i ? { ...x, kind: d.value as EntryFieldDto["kind"] } : x,
                      ),
                    })
                  }
                >
                  {(Object.keys(KIND_LABELS) as EntryFieldDto["kind"][]).map((k) => (
                    <option key={k} value={k}>
                      {KIND_LABELS[k]}
                    </option>
                  ))}
                </Select>
                <Input
                  size="small"
                  type={f.kind === "password" ? "password" : "text"}
                  style={{ flex: 1 }}
                  placeholder="值"
                  value={f.value}
                  onChange={(_, d) =>
                    setEditor({
                      ...editor,
                      fields: editor.fields.map((x, j) => (j === i ? { ...x, value: d.value } : x)),
                    })
                  }
                />
                <Button
                  appearance="subtle"
                  size="small"
                  icon={<DismissRegular />}
                  onClick={() =>
                    setEditor({ ...editor, fields: editor.fields.filter((_, j) => j !== i) })
                  }
                />
              </div>
            ))}
            <Button
              size="small"
              icon={<AddRegular />}
              onClick={() =>
                setEditor({
                  ...editor,
                  fields: [...editor.fields, { key: "", kind: "text", value: "" }],
                })
              }
            >
              添加字段
            </Button>
            <Divider />
            <Text className={styles.muted}>密码生成器</Text>
            <div className={styles.row}>
              <span className={styles.muted}>长度</span>
              <SpinButton
                aria-label="密码长度"
                size="small"
                min={GEN_LEN.min}
                max={GEN_LEN.max}
                step={1}
                style={{ width: TIER_W.s }}
                value={editor.genLength}
                onChange={(_, d) => {
                  // SpinButton 输入非法文本时给 NaN，夹之前先验（生成器策略是后端入参边界）
                  const len = d.value ?? Number.NaN;
                  setEditor({
                    ...editor,
                    genLength: Number.isFinite(len)
                      ? Math.min(GEN_LEN.max, Math.max(GEN_LEN.min, len))
                      : 16,
                  });
                }}
              />
              <Checkbox
                label="大写"
                checked={editor.genUpper}
                onChange={(_, d) => setEditor({ ...editor, genUpper: !!d.checked })}
              />
              <Checkbox
                label="小写"
                checked={editor.genLower}
                onChange={(_, d) => setEditor({ ...editor, genLower: !!d.checked })}
              />
              <Checkbox
                label="数字"
                checked={editor.genDigits}
                onChange={(_, d) => setEditor({ ...editor, genDigits: !!d.checked })}
              />
              <Checkbox
                label="符号"
                checked={editor.genSymbols}
                onChange={(_, d) => setEditor({ ...editor, genSymbols: !!d.checked })}
              />
              <Checkbox
                label="避免易混淆"
                checked={editor.genAmbiguous}
                onChange={(_, d) => setEditor({ ...editor, genAmbiguous: !!d.checked })}
              />
              <Button
                size="small"
                icon={<PasswordRegular />}
                onClick={() => void doGen()}
                title="填充到首个密码字段"
              >
                生成
              </Button>
            </div>
            <Divider />
            <Input
              placeholder="TOTP 密钥（base32，可选）"
              value={editor.totpSecret}
              onChange={(_, d) => setEditor({ ...editor, totpSecret: d.value })}
            />
            <InlineError text={formErr} />
          </DrawerBody>
          <DrawerFooter>
            <Button onClick={() => void closeEditor()}>取消</Button>
            <Button appearance="primary" disabled={busy} onClick={() => void doSaveEntry()}>
              {busy ? "保存中…" : "保存"}
            </Button>
          </DrawerFooter>
        </Drawer>
      )}

      {/* T-B1-3 改主密码：三字段皆 password（永不回显），错误内联不蒸发 */}
      <Dialog open={mpOpen} onOpenChange={(_, d) => !d.open && !busy && setMpOpen(false)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>修改主密码</DialogTitle>
            <DialogContent>
              <Text className={styles.muted}>
                旧密码验证后用新密码重新包裹数据密钥（DEK），条目密文不搬运。
              </Text>
              <div style={{ display: "flex", flexDirection: "column", gap: "10px", marginTop: "8px" }}>
                <Input
                  type="password"
                  aria-label="当前主密码"
                  placeholder="当前主密码"
                  value={mpOld}
                  onChange={(_, d) => setMpOld(d.value)}
                />
                <Input
                  type="password"
                  aria-label="新主密码"
                  placeholder="新主密码（≥8 位）"
                  value={mpNew}
                  onChange={(_, d) => setMpNew(d.value)}
                />
                <Input
                  type="password"
                  aria-label="确认新主密码"
                  placeholder="确认新主密码"
                  value={mpConfirm}
                  onChange={(_, d) => setMpConfirm(d.value)}
                />
              </div>
              <InlineError text={mpErr} />
            </DialogContent>
            <DialogActions>
              <Button appearance="subtle" disabled={busy} onClick={() => setMpOpen(false)}>
                取消
              </Button>
              <Button appearance="primary" disabled={busy} onClick={() => void doChangePassword()}>
                {busy ? "修改中…" : "确认修改"}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
}
