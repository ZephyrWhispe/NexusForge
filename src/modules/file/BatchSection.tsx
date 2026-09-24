import { useState } from "react";
import {
  Badge,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Input,
  Select,
  Text,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import {
  fileRenameApply,
  fileRenamePlan,
  parseAppError,
  type FileEntryDto,
  type RenameCaseDto,
  type RenamePlanDto,
} from "../../ipc/client";
import { notify } from "../../stores/notifications";
import InlineError from "../../components/InlineError";
import DeferredBadge from "../../components/DeferredBadge";

/**
 * 批量工具档（T-B7-27，panels/04 §2「批量工具」行）：批量重命名工作台
 * （规则表单→实时预览表→勾选应用）与压缩/解压入口自 FilePanel browse 档
 * 逐行挪来（F7 判据一字未动）。选中集与「目标目录」输入仍住「文件」档
 * （FilePanel 父级状态跨档常驻），本档对当前选择执行——两档共用同一份
 * 选择事实源，不在这里复制第二份选中集。
 */

const useStyles = makeStyles({
  toolbar: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  rnField: { display: "flex", flexDirection: "column", gap: "2px" },
  rnPlanRow: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    minWidth: 0,
    fontSize: tokens.fontSizeBase200,
  },
  rnPath: { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxWidth: "180px" },
});

/**
 * 冲突原因前端推导（T-B1-5）：后端 conflict 是单一布尔、两种成因不可分辨
 * （rename.rs:141-149 目标已存在 || 计划内重复），故"表内重复"由计划内同名
 * 目标计数（>1）判出，其余冲突如实标"目标已存在"。键=from 路径。
 */
export function renameConflictReasons(plans: RenamePlanDto[]): Map<string, string> {
  const targetCount = new Map<string, number>();
  for (const p of plans) {
    if (p.from === p.to) continue;
    const k = p.to.toLowerCase();
    targetCount.set(k, (targetCount.get(k) ?? 0) + 1);
  }
  const out = new Map<string, string>();
  for (const p of plans) {
    if (!p.conflict) continue;
    out.set(p.from, (targetCount.get(p.to.toLowerCase()) ?? 0) > 1 ? "表内重复" : "目标已存在");
  }
  return out;
}

/**
 * zip 目标路径推导：目标输入留空 → 当前目录\<主名>.zip；以 \ 结尾或裸盘符
 * 视作目录拼自动名；其余按完整 zip 文件路径原样使用（run_compress 的 dst 是
 * zip 文件本体而非目录，ops.rs:916）。
 */
export function zipTarget(cwd: string, dstInput: string, stem: string): string {
  const d = dstInput.trim();
  const name = `${stem}.zip`;
  const base = cwd.replace(/\\+$/, "");
  if (!d) return `${base}\\${name}`;
  if (/\\$/.test(d)) return `${d}${name}`;
  if (/^[A-Za-z]:$/.test(d)) return `${d}\\${name}`;
  return d;
}

export default function BatchSection({
  cwd,
  entries,
  selected,
  error,
  onReload,
  onCompress,
  onExtract,
}: {
  cwd: string | null;
  entries: FileEntryDto[];
  selected: Set<string>;
  error: string | null;
  onReload: (path: string) => void;
  onCompress: () => void;
  onExtract: () => void;
}) {
  const styles = useStyles();
  // ---- T-B1-5 批量重命名（随本档挪来）----
  const [rnOpen, setRnOpen] = useState(false);
  const [rnTemplate, setRnTemplate] = useState("{name}{ext}");
  const [rnRegex, setRnRegex] = useState("");
  const [rnReplacement, setRnReplacement] = useState("");
  const [rnCase, setRnCase] = useState<RenameCaseDto>("none");
  const [rnStart, setRnStart] = useState("1");
  const [rnBusy, setRnBusy] = useState(false);
  const [rnPlans, setRnPlans] = useState<RenamePlanDto[] | null>(null);
  const [rnErr, setRnErr] = useState<string | null>(null);
  const [rnChecked, setRnChecked] = useState<Set<string>>(new Set());

  // ---- 批量重命名 Dialog（F7：预览→勾选→应用，冲突条目后端兜底跳过）----
  const openRename = () => {
    setRnPlans(null);
    setRnErr(null);
    setRnOpen(true);
  };

  const doRenamePlan = async () => {
    if (!cwd) return;
    // 只交文件主名：显式 names 不做目录过滤（rename.rs:78-86），目录必须由前端挡下
    const names = entries.filter((e) => selected.has(e.path) && !e.is_dir).map((e) => e.name);
    if (selected.size > 0 && names.length === 0) {
      setRnErr("所选条目全是目录：仅文件参与批量重命名");
      return;
    }
    const parsed = Number.parseInt(rnStart, 10);
    setRnBusy(true);
    setRnErr(null);
    setRnPlans(null);
    try {
      const plans = await fileRenamePlan(cwd, names, {
        template: rnTemplate,
        regex: rnRegex.trim() ? rnRegex : null,
        replacement: rnReplacement,
        case: rnCase,
        start: Number.isNaN(parsed) || parsed < 0 ? 1 : parsed,
      });
      setRnPlans(plans);
      // 默认只勾非冲突、非 no-op 条目（服务端对冲突条目也会再次跳过，双保险）
      setRnChecked(new Set(plans.filter((p) => !p.conflict && p.from !== p.to).map((p) => p.from)));
    } catch (e) {
      const err = parseAppError(e);
      setRnErr(err ? `${err.data.code}: ${err.data.message}` : "生成重命名预览失败");
    } finally {
      setRnBusy(false);
    }
  };

  const doRenameApply = async () => {
    if (!cwd || !rnPlans) return;
    const checked = rnPlans.filter((p) => rnChecked.has(p.from));
    if (checked.length === 0) {
      setRnErr("未勾选任何可执行条目");
      return;
    }
    try {
      const n = await fileRenameApply(checked);
      notify("success", "批量重命名完成", `已重命名 ${n} 项；冲突与未勾选条目未执行。`);
      setRnOpen(false);
      setRnPlans(null);
      onReload(cwd);
    } catch (e) {
      const err = parseAppError(e);
      setRnErr(err ? `${err.data.code}: ${err.data.message}` : "应用重命名失败");
    }
  };

  const toggleRn = (from: string) => {
    setRnChecked((prev) => {
      const next = new Set(prev);
      if (next.has(from)) next.delete(from);
      else next.add(from);
      return next;
    });
  };

  const rnReasons = rnPlans ? renameConflictReasons(rnPlans) : null;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "12px" }}>
      <div className={styles.toolbar}>
        <Button size="small" onClick={openRename}>
          批量重命名
        </Button>
        <Button size="small" appearance="secondary" disabled={selected.size === 0} onClick={onCompress}>
          压缩为 zip
        </Button>
        <Button size="small" appearance="secondary" disabled={selected.size !== 1} onClick={onExtract}>
          解压
        </Button>
        {/* T-B6-13 明示不做（09 §6.3 双向钉）：徽标只说"没做"，不扮"禁用的就绪" */}
        <DeferredBadge label="diff/镜像工作台" decisionRef="09 §6.3-(d)" />
        <span style={{ flex: 1 }} />
        <InlineError text={error} />
      </div>
      <Text size={200} className={styles.muted}>
        已选 {selected.size} 项：条目在「文件」档点选，「目标目录」输入也在「文件」档；
        本档对当前选择执行批量重命名与压缩/解压（跨档共用同一份选择，不另立第二套）。
      </Text>

      {/* 批量重命名 Dialog（F7：规则表单 → 预览表 → 勾选应用；冲突原因前端推导） */}
      <Dialog open={rnOpen} onOpenChange={(_, d) => !d.open && setRnOpen(false)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>批量重命名</DialogTitle>
            <DialogContent>
              <div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
                <Text size={200} className={styles.muted}>
                  仅文件，目录不参与。
                  {selected.size === 0
                    ? "未选中文件：将对当前目录全部文件生成计划。"
                    : `已选 ${entries.filter((e) => selected.has(e.path) && !e.is_dir).length} 个文件参与。`}
                </Text>
                <div className={styles.rnField}>
                  <Text size={200} className={styles.muted}>
                    模板（变量仅 {"{name}"} {"{ext}"} {"{n}"} {"{n:0N}"}，N≤10）
                  </Text>
                  <Input
                    size="small"
                    aria-label="重命名模板"
                    value={rnTemplate}
                    onChange={(_, d) => setRnTemplate(d.value)}
                  />
                </div>
                <div className={styles.rnField}>
                  <Text size={200} className={styles.muted}>
                    正则（作用于主名，留空不处理）
                  </Text>
                  <Input
                    size="small"
                    aria-label="重命名正则"
                    value={rnRegex}
                    onChange={(_, d) => setRnRegex(d.value)}
                  />
                </div>
                <div className={styles.rnField}>
                  <Text size={200} className={styles.muted}>
                    替换串（$1 组引用）
                  </Text>
                  <Input
                    size="small"
                    aria-label="重命名替换串"
                    value={rnReplacement}
                    onChange={(_, d) => setRnReplacement(d.value)}
                  />
                </div>
                <div className={styles.rnField}>
                  <Text size={200} className={styles.muted}>
                    大小写
                  </Text>
                  <Select
                    size="small"
                    aria-label="大小写转换"
                    value={rnCase}
                    onChange={(_, d) => setRnCase((d.value || "none") as RenameCaseDto)}
                  >
                    <option value="none">不转换</option>
                    <option value="lower">全部小写</option>
                    <option value="upper">全部大写</option>
                  </Select>
                </div>
                <div className={styles.rnField}>
                  <Text size={200} className={styles.muted}>
                    序号起始值
                  </Text>
                  <Input
                    size="small"
                    type="number"
                    aria-label="序号起始值"
                    value={rnStart}
                    onChange={(_, d) => setRnStart(d.value)}
                    style={{ maxWidth: "100px" }}
                  />
                </div>
                {rnErr && <InlineError text={rnErr} />}
                {rnBusy && (
                  <Text size={200} role="status" className={styles.muted}>
                    正在生成预览…
                  </Text>
                )}
                {rnPlans &&
                  rnPlans.map((p) => (
                    <div key={p.from} className={styles.rnPlanRow}>
                      <Checkbox
                        checked={rnChecked.has(p.from)}
                        onChange={() => toggleRn(p.from)}
                        aria-label={`选择 ${p.from}`}
                      />
                      <span className={styles.rnPath} title={p.from}>
                        {p.from.split(/[\\/]/).pop()}
                      </span>
                      <span>→</span>
                      <span className={styles.rnPath} title={p.to}>
                        {p.to.split(/[\\/]/).pop()}
                      </span>
                      {p.from === p.to && <Badge appearance="outline">不变</Badge>}
                      {rnReasons?.get(p.from) && (
                        <Badge appearance="tint" color="warning">
                          {rnReasons.get(p.from)}
                        </Badge>
                      )}
                    </div>
                  ))}
                {rnPlans && rnPlans.length === 0 && (
                  <Text size={200} className={styles.muted}>
                    计划为 0 条
                  </Text>
                )}
              </div>
            </DialogContent>
            <DialogActions>
              <Button onClick={() => void doRenamePlan()}>生成预览</Button>
              <Button
                appearance="primary"
                disabled={!rnPlans || rnBusy}
                onClick={() => void doRenameApply()}
              >
                应用（勾选 {rnChecked.size} 项）
              </Button>
              <Button appearance="subtle" onClick={() => setRnOpen(false)}>
                关闭
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
}
