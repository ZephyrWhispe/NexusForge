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
  Input,
  Select,
  Spinner,
} from "@fluentui/react-components";
import {
  PasswordRegular,
  LockClosedRegular,
  AddRegular,
  DeleteRegular,
  CopyRegular,
  DismissRegular,
  EyeRegular,
  EyeOffRegular,
} from "@fluentui/react-icons";
import {
  parseAppError,
  vaultCreate,
  vaultEntries,
  vaultEntryAdd,
  vaultEntryDelete,
  vaultEntryUpdate,
  vaultFolderCreate,
  vaultFolderDelete,
  vaultFolders,
  vaultGeneratePassword,
  vaultLock,
  vaultStatus,
  vaultTotpNow,
  vaultUnlock,
  type EntryFieldDto,
  type PasswordPolicyDto,
  type VaultEntryDto,
  type VaultFolderDto,
  type VaultStatusDto,
} from "../../ipc/client";
import { reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import { keyActivate } from "../../a11y";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";

/**
 * 密码库面板（docs/impl/05 V7，M5 v1）三态渲染：
 * ① uninitialized 建库（Argon2 64MiB，秒级）② locked 解锁（5 次错误 300s 冷却）
 * ③ unlocked 条目管理（文件夹 + 条目 + TOTP + 生成器）。vault.* 事件驱动刷新。
 */
const useStyles = makeStyles({
  root: {
    flex: 1,
    minWidth: 0,
    overflowY: "auto",
    padding: "0 20px 20px",
    display: "flex",
    flexDirection: "column",
    gap: "16px",
  },
  center: {
    flex: 1,
    display: "grid",
    placeItems: "center",
  },
  card: {
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    padding: "24px 28px",
    backgroundColor: tokens.colorNeutralBackground1,
    display: "flex",
    flexDirection: "column",
    gap: "12px",
    width: "420px",
  },
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
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
    const tick = async () => {
      try {
        const [code, rem] = await vaultTotpNow(secret);
        if (!alive) return;
        setOtp(code);
        setRemaining(rem);
      } catch {
        if (alive) setOtp("------");
      }
    };
    void tick();
    const t = setInterval(tick, 1000);
    return () => {
      alive = false;
      clearInterval(t);
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

function FieldValue({ field }: { field: EntryFieldDto }) {
  const styles = useStyles();
  const [shown, setShown] = useState(false);
  const copy = () =>
    void navigator.clipboard.writeText(field.value).catch((e) => reportError(e, { context: "复制到剪贴板失败" }));
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
        title="复制"
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

export default function VaultPanel() {
  const styles = useStyles();
  const [status, setStatus] = useState<VaultStatusDto | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [formErr, setFormErr] = useState<string | null>(null);

  // 建库/解锁表单
  const [pw, setPw] = useState("");
  const [pwConfirm, setPwConfirm] = useState("");
  const [lockoutLeft, setLockoutLeft] = useState(0);

  // 解锁态数据
  const [folders, setFolders] = useState<VaultFolderDto[]>([]);
  const [activeFolder, setActiveFolder] = useState<string | "all">("all");
  const [entries, setEntries] = useState<VaultEntryDto[]>([]);
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [newFolderName, setNewFolderName] = useState("");
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
          vaultEntries(activeFolder === "all" ? null : activeFolder, null),
        ]);
        setFolders(f);
        setEntries(e);
      }
    } catch (err) {
      setLoadErr(errText(err));
    } finally {
      setLoaded(true);
    }
  }, [activeFolder]);
  reloadRef.current = () => void refresh();

  useEffect(() => {
    reloadRef.current();
  }, [activeFolder]);

  // 冷却倒计时（锁定态每秒递减）
  useEffect(() => {
    if (status?.state !== "locked" || lockoutLeft <= 0) return;
    const t = setInterval(() => setLockoutLeft((v) => (v > 0 ? v - 1 : 0)), 1000);
    return () => clearInterval(t);
  }, [status?.state, lockoutLeft]);

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
          folder_id: activeFolder === "all" ? null : activeFolder,
          favorite: editor.favorite,
          totp_secret: totp,
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

  if (status.state === "uninitialized") {
    return (
      <div className={styles.root}>
        <div className={styles.center}>
          <div className={styles.card}>
            <Text size={400} weight="semibold">创建保险库</Text>
            <Text className={styles.muted}>
              主密码经 Argon2id（64MiB 内存参数）派生密钥，用于加密全部条目。创建约需数秒。
            </Text>
            <Input
              type="password"
              placeholder="主密码（≥8 位）"
              value={pw}
              onChange={(_, d) => setPw(d.value)}
            />
            <Input
              type="password"
              placeholder="确认主密码"
              value={pwConfirm}
              onChange={(_, d) => setPwConfirm(d.value)}
            />
            <InlineError text={formErr} />
            <Button appearance="primary" disabled={busy} onClick={() => void doCreate()}>
              {busy ? "正在创建…" : "创建保险库"}
            </Button>
          </div>
        </div>
      </div>
    );
  }

  if (status.state === "locked") {
    return (
      <div className={styles.root}>
        <div className={styles.center}>
          <div className={styles.card}>
            <Text size={400} weight="semibold">
              <LockClosedRegular /> 密码库已锁定
            </Text>
            <Text className={styles.muted}>
              输入主密码解锁。连续错误 5 次将进入 300 秒冷却。
            </Text>
            <Input
              type="password"
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
          </div>
        </div>
      </div>
    );
  }

  // ---- unlocked：条目管理 ----
  return (
    <div className={styles.root}>
      <div className={styles.row}>
        <Button
          appearance="primary"
          icon={<AddRegular />}
          onClick={() => setEditor(emptyEditor())}
        >
          新建条目
        </Button>
        <Button icon={<LockClosedRegular />} onClick={() => void doLock()}>
          立即锁定
        </Button>
        <Text className={styles.muted}>共 {entries.length} 条 · 字段已 AES-256-GCM 加密</Text>
      </div>

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
              <Text truncate>{f.name}</Text>
              <Button
                appearance="subtle"
                size="small"
                icon={<DeleteRegular />}
                title="删除文件夹"
                onClick={(e) => {
                  e.stopPropagation();
                  void doDeleteFolder(f);
                }}
              />
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
            <Button size="small" icon={<AddRegular />} onClick={() => void doAddFolder()} />
          </div>
        </div>

        <div className={styles.entryList}>
          {(loaded && entries.length === 0) && (
            <EmptyState text="暂无条目，点击「新建条目」添加。" />
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
                  onClick={() => void doDeleteEntry(entry)}
                />
              </div>
              {entry.fields.map((f, i) => (
                <FieldValue key={i} field={f} />
              ))}
              {entry.totp_secret && <TotpBadge secret={entry.totp_secret} />}
            </div>
          ))}
        </div>
      </div>

      {editor && (
        // 条目编辑对话框（D-18）：Fluent Dialog 自带焦点陷阱/Esc/遮罩关闭；
        // 所有关闭路径统一走 closeEditor()，未保存修改先经 ConfirmDialog 确认
        <Dialog open onOpenChange={() => void closeEditor()}>
          <DialogSurface>
            <DialogBody>
              <DialogTitle>{editor.entry ? "编辑条目" : "新建条目"}</DialogTitle>
              <DialogContent>
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
                  style={{ width: "110px" }}
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
                  style={{ width: "110px" }}
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
              <Input
                size="small"
                type="number"
                style={{ width: "70px" }}
                value={String(editor.genLength)}
                onChange={(_, d) =>
                  setEditor({ ...editor, genLength: Math.max(4, Number(d.value) || 16) })
                }
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
              </DialogContent>
              <DialogActions>
                <Button onClick={() => void closeEditor()}>取消</Button>
                <Button appearance="primary" disabled={busy} onClick={() => void doSaveEntry()}>
                  {busy ? "保存中…" : "保存"}
                </Button>
              </DialogActions>
            </DialogBody>
          </DialogSurface>
        </Dialog>
      )}
    </div>
  );
}
